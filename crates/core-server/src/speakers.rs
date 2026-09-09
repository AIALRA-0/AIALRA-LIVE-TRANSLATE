//! Course-local anonymous labels derived from private, completed ASR job results.
//! No vector goes into public events; ordinary course purge already removes jobs.

use crate::app::AppState;
use aialra_event_protocol::EventEnvelope;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

const MODEL: &str = "campplus-zh-en-16k-v1";

#[derive(Debug, Deserialize)]
pub struct Observation {
    pub status: String,
    pub model: String,
    pub provider: String,
    #[serde(default)]
    pub embedding: Vec<f32>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Label {
    pub status: String,
    pub index: Option<u32>,
}

impl Label {
    pub fn unconfirmed() -> Self {
        Self {
            status: "unconfirmed".into(),
            index: None,
        }
    }
    fn assigned(index: u32) -> Self {
        Self {
            status: "assigned".into(),
            index: Some(index),
        }
    }
}

fn normalized(observation: &Observation) -> Option<Vec<f32>> {
    if observation.status != "observed"
        || observation.model != MODEL
        || observation.provider != "cpu"
        || observation.embedding.len() != 192
        || observation.embedding.iter().any(|v| !v.is_finite())
    {
        return None;
    }
    let norm = observation
        .embedding
        .iter()
        .map(|v| v * v)
        .sum::<f32>()
        .sqrt();
    if !norm.is_finite() || !(0.9..=1.1).contains(&norm) {
        return None;
    }
    Some(observation.embedding.iter().map(|v| v / norm).collect())
}

fn choose(current: &[f32], profiles: &BTreeMap<u32, Vec<f32>>, next: u32) -> Label {
    let mut scores = profiles
        .iter()
        .map(|(&index, profile)| {
            (
                index,
                current.iter().zip(profile).map(|(a, b)| a * b).sum::<f32>(),
            )
        })
        .collect::<Vec<_>>();
    scores.sort_by(|a, b| b.1.total_cmp(&a.1));
    if let Some(&(index, best)) = scores.first() {
        let runner_up = scores.get(1).map_or(-1.0, |v| v.1);
        if best >= 0.5 && best - runner_up >= 0.08 {
            return Label::assigned(index);
        }
        if best >= 0.4 {
            return Label::unconfirmed();
        }
    }
    if next <= 64 {
        Label::assigned(next)
    } else {
        Label::unconfirmed()
    }
}

pub fn label_for_asr(
    state: &AppState,
    session_id: &str,
    job_id: &str,
    observation: Option<&Observation>,
) -> Result<Option<Label>> {
    let Some(observation) = observation else {
        return Ok(None);
    };
    if observation.status == "disabled" {
        return Ok(None);
    }
    let events = state.store.list_events(session_id)?;
    let segments = events
        .iter()
        .filter(|e| e.event_type == "segment.finalized")
        .collect::<Vec<_>>();
    // A retried result cannot renumber a segment that was already committed.
    if let Some(existing) = segments.iter().find(|e| e.correlation_id == job_id) {
        return Ok(existing
            .payload
            .get("speaker")
            .cloned()
            .and_then(|value| serde_json::from_value(value).ok()));
    }
    let Some(current) = normalized(observation) else {
        return Ok(Some(Label::unconfirmed()));
    };
    let mut profiles = BTreeMap::new();
    let mut next = 1;
    for event in segments {
        let Some(index) = event.payload["speaker"]["index"]
            .as_u64()
            .filter(|i| (1..=64).contains(i))
            .map(|i| i as u32)
        else {
            continue;
        };
        next = next.max(index + 1);
        if profiles.contains_key(&index) {
            continue;
        }
        let Some(job) = state.store.get_model_job(&event.correlation_id)? else {
            continue;
        };
        if job.session_id != session_id || job.status != "completed" {
            continue;
        }
        let Some(raw) = job
            .result
            .as_ref()
            .and_then(|v| v.get("speaker_observation"))
        else {
            continue;
        };
        let Ok(reference) = serde_json::from_value::<Observation>(raw.clone()) else {
            continue;
        };
        if let Some(vector) = normalized(&reference) {
            profiles.insert(index, vector);
        }
    }
    Ok(Some(choose(&current, &profiles, next)))
}

pub fn paragraph_label<'a>(events: impl Iterator<Item = &'a EventEnvelope>) -> Option<Label> {
    let values = events.map(|e| e.payload.get("speaker")).collect::<Vec<_>>();
    if values.iter().all(|v| v.is_none_or(|value| value.is_null())) {
        return None;
    }
    let labels = values
        .into_iter()
        .map(|v| {
            v.cloned()
                .and_then(|v| serde_json::from_value::<Label>(v).ok())
        })
        .collect::<Vec<_>>();
    let Some(Some(first)) = labels.first() else {
        return Some(Label::unconfirmed());
    };
    if first.status == "assigned"
        && first.index.is_some()
        && labels.iter().all(|v| v.as_ref() == Some(first))
    {
        Some(first.clone())
    } else {
        Some(Label::unconfirmed())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aialra_event_store::{NewModelJob, NewSession};
    use serde_json::json;

    fn vector(index: usize) -> Vec<f32> {
        let mut v = vec![0.0; 192];
        v[index] = 1.0;
        v
    }

    #[test]
    fn labels_match_known_voice_and_preserve_ambiguity() {
        let profiles = BTreeMap::from([(1, vector(0)), (2, vector(1))]);
        assert_eq!(choose(&vector(0), &profiles, 3), Label::assigned(1));
        assert_eq!(choose(&vector(2), &profiles, 3), Label::assigned(3));
        let mut ambiguous = vec![0.0; 192];
        ambiguous[0] = 0.7;
        ambiguous[1] = 0.7;
        assert_eq!(choose(&ambiguous, &profiles, 3), Label::unconfirmed());
        assert_eq!(choose(&vector(2), &profiles, 65), Label::unconfirmed());
    }

    #[test]
    fn invalid_or_unknown_model_vectors_cannot_create_identities() {
        let mut observation = Observation {
            status: "observed".into(),
            model: MODEL.into(),
            provider: "cpu".into(),
            embedding: vector(0),
        };
        assert!(normalized(&observation).is_some());
        observation.embedding[0] = f32::NAN;
        assert!(normalized(&observation).is_none());
        observation.embedding = vector(0);
        observation.model = "other-model".into();
        assert!(normalized(&observation).is_none());
        observation.model = MODEL.into();
        observation.embedding = vec![0.0; 192];
        assert!(normalized(&observation).is_none());
    }

    fn commit_voice(state: &AppState, session: &str, key: &str, voice: usize) -> Label {
        let observed = json!({"status": "observed", "model": MODEL, "provider": "cpu",
                              "embedding": vector(voice)});
        let observation: Observation = serde_json::from_value(observed.clone()).unwrap();
        let job = state
            .enqueue_job(NewModelJob {
                id: key.into(),
                session_id: session.into(),
                job_type: "asr".into(),
                priority: 100,
                input: json!({}),
                input_object_hash: None,
                idempotency_key: key.into(),
            })
            .unwrap();
        let leased = state
            .store
            .lease_model_job_for("fixture", &["asr".into()], 60, Some(&job.id))
            .unwrap()
            .unwrap();
        let label = label_for_asr(state, session, key, Some(&observation))
            .unwrap()
            .unwrap();
        state.emit_idempotent(key, session, "fixture", "segment.finalized", 0, key, None,
            json!({"segment_id": format!("seg_{key}"), "text": "Synthetic source", "speaker": label}))
            .unwrap();
        state
            .store
            .complete_model_job(
                &leased.id,
                "fixture",
                &json!({"speaker_observation": observed}),
            )
            .unwrap();
        label
    }

    #[test]
    fn private_profiles_survive_reopen_without_cross_course_matching_or_public_vectors() {
        let temp = tempfile::tempdir().unwrap();
        let state = AppState::open(temp.path()).unwrap();
        for id in ["course-speaker-a", "course-speaker-b"] {
            state
                .store
                .create_session(&NewSession {
                    id: id.into(),
                    title: "Synthetic voices".into(),
                    source_language: "en".into(),
                    target_language: "zh-CN".into(),
                    privacy_mode: "local_only".into(),
                    consent_confirmed: true,
                    demo_mode: false,
                })
                .unwrap();
        }
        assert_eq!(
            commit_voice(&state, "course-speaker-a", "voice-job-1", 0),
            Label::assigned(1)
        );
        assert_eq!(
            commit_voice(&state, "course-speaker-a", "voice-job-2", 1),
            Label::assigned(2)
        );
        drop(state);
        let state = AppState::open(temp.path()).unwrap();
        assert_eq!(
            commit_voice(&state, "course-speaker-a", "voice-job-3", 0),
            Label::assigned(1)
        );
        assert_eq!(
            commit_voice(&state, "course-speaker-b", "voice-job-4", 1),
            Label::assigned(1)
        );
        let events = state.store.list_events("course-speaker-a").unwrap();
        let public = serde_json::to_string(&events).unwrap();
        assert!(!public.contains("embedding"));
        assert!(!public.contains(MODEL));
        let segments = events
            .iter()
            .filter(|e| e.event_type == "segment.finalized");
        assert_eq!(
            paragraph_label(segments.clone()),
            Some(Label::unconfirmed())
        );
        assert_eq!(paragraph_label(segments.take(1)), Some(Label::assigned(1)));
        let changed = Observation {
            status: "observed".into(),
            model: MODEL.into(),
            provider: "cpu".into(),
            embedding: vector(1),
        };
        assert_eq!(
            label_for_asr(&state, "course-speaker-a", "voice-job-1", Some(&changed)).unwrap(),
            Some(Label::assigned(1))
        );
    }
}
