// EventEnvelope mirrors the versioned Rust protocol and remains permissive for new payload fields.
export interface EventEnvelope {
  schema_version: string;
  event_id: string;
  session_id: string;
  source_id: string;
  sequence: number;
  event_type: string;
  captured_at_monotonic_ns: number;
  captured_at_wall: string;
  ingested_at: string;
  correlation_id: string;
  causation_id: string | null;
  payload: Record<string, unknown>;
  content_hash: string;
}

// Session is the durable recording state returned by the local control service.
export interface Session {
  id: string;
  title: string;
  source_language: string;
  target_language: string;
  privacy_mode: string;
  consent_confirmed: boolean;
  demo_mode: boolean;
  state: string;
  created_at: string;
  updated_at: string;
}

export interface Project {
  id: string;
  owner_subject: string;
  title: string;
  source_language: string;
  target_language: string;
  version: number;
  created_at: string;
  updated_at: string;
}

export interface ProjectUpdate {
  cursor: number;
  project_id: string;
  session_id: string | null;
  update_type: string;
  payload: Record<string, unknown>;
  created_at: string;
}

export interface RecordingLease {
  project_id: string;
  session_id: string;
  holder_device_id: string;
  generation: number;
  expires_at: string;
  lease_token: string;
}

export interface ReadWeaveStatus {
  configured: boolean;
  queued: number;
  syncing: number;
  completed: number;
  conflicts: number;
  updated_at: string | null;
  note_url: string | null;
}

export interface ReadWeavePreview {
  sessions: Array<{
    session_id: string;
    title: string;
    state: string;
    latest_entries: Array<{ segment_id: string; original: string; translation: string | null }>;
    explanation_count: number;
  }>;
}

// TimelineItem groups revisions under stable segment, card, or page identifiers.
export interface TimelineItem {
  id: string;
  kind: "segment" | "translation" | "explanation" | "asset" | "status";
  title: string;
  body: string;
  evidenceIds: string[];
  occurredAt: string;
  provider?: string;
  imageUrl?: string;
}
