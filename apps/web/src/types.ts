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
