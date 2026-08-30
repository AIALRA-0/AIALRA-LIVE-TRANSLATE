<div align="center">

<h1 align="center">AIALRA-LIVE-TRANSLATE</h1>

Keep one course project synchronized across devices while one recorder sends durable audio to a trusted local GPU for transcripts, Chinese translations, explanations, and ReadWeave notes

[中文](README.md) · [Deployment](deploy/README.md) · [Privacy boundaries](docs/PRIVACY_BOUNDARIES.md) · [Validation](docs/VALIDATION_REPORT.md)

`Public beta` · `Continuous workspace` · `Local first` · `RTX CUDA verified` · `ReadWeave`

![A real RTX 4080 workspace with a project tree, continuous bilingual paragraphs, recording state, and live GPU telemetry](docs/assets/readme/real-gpu-workspace.png)

Figure 1  Public synthetic lecture audio processed by `faster-whisper:small@cuda` and `ollama:qwen2.5:3b-instruct@cuda`, with a persistent workspace tree, continuous bilingual paragraphs, recording state, RTX 4080 telemetry, and stripped metadata

</div>

## 1. The problem

A student listening to an English lecture should not also have to transcribe, translate, look up unfamiliar terms, organize slides, and preserve notes at the same time

AIALRA puts those actions on one append-only course timeline:

- Show the same owner-scoped project, sessions, recording state, and results on every signed-in device
- Allow one active recorder per project while other devices observe and can take over only after lease expiry
- Capture a microphone, browser tab, or shared system audio from a desktop or mobile browser
- Commit unacknowledged browser audio and its next sequence in IndexedDB so refreshes and short outages can resume safely
- Persist every audio block before acknowledging it, then rebuild unfinished ASR windows from durable chunks after a Core restart
- Keep ingress, authentication, durable jobs, and events on the server while a trusted RTX GPU Agent claims model work
- Preserve revisions instead of silently overwriting captions, translations, explanations, or corrections
- Add PPTX, PDF, DOCX, image, and text material to later explanations with segment or page evidence
- Project stable text and evidence into ReadWeave over private ETAPI without overwriting user notes or content outside managed markers
- Use DingTalk A1 as a parallel recorder and post-session recovery source until public evidence establishes continuous third-party PCM access

This is not a covert recording tool  A real session requires confirmed permission and must follow instructor, institutional, and applicable legal requirements

## 2. Verified runtime architecture

```mermaid
flowchart TD
  Recorder[Recording browser] -->|HTTPS + WSS + lease| Auth[Authentik-protected ingress]
  Observer[Observer device] -->|Project SSE| Auth
  Android[Android foreground capture] -->|WSS + ACK + lease| Auth
  A1[DingTalk A1] -->|Parallel record and recovery| Core
  Auth --> Core[Rust project, audio, and event core]
  Core --> Store[(SQLite WAL + content-addressed files)]
  Core --> Queue[Durable model job queue]
  Agent[Windows RTX GPU Agent] -->|Private outbound lease| Queue
  Agent --> ASR[faster-whisper small CUDA float16]
  Agent --> Translate[Ollama 3B translation]
  Agent --> Explain[Ollama 3B rolling explanation]
  Agent --> Summary[Ollama 7B final summary]
  Agent --> Vision[Qwen3-VL 8B image understanding]
  Core --> Notes[ReadWeave private ETAPI projection]
  Store --> Observer
```

Audio ingest, durable storage, acknowledgement, and stop control remain independent from model availability

## 3. First local run

Prerequisites: Windows, Rust 1.95, Node.js 22, pnpm 10, Python 3.12, uv, NVIDIA CUDA, and Ollama

```powershell
ollama pull qwen2.5:3b-instruct # Download the real-time translation model
ollama pull qwen2.5:7b-instruct # Download the final-summary model
ollama pull qwen3-vl:8b-instruct # Download the local image-understanding model
Copy-Item .env.example .env # Create a local configuration from reserved example values
./scripts/start-local.ps1 # Build and start the workspace, Core, model Worker, and GPU Agent
```

The supervisor verifies local models and starts Ollama when needed, while the browser remains on one workspace page

The production UI has no default demo session and no deterministic model fallback  Confirm recording permission, select one audio source, start the course, then use the visible stop control

Remote capture requires HTTPS  Users stay on the same page and do not enter an API address

## 4. Capability status

| Capability | Status | Evidence boundary |
|---|---|---|
| Projects and device sync | Available | Authentik stable owner ID, owner isolation, project SSE cursor recovery, Chrome and Edge two-device observation |
| Exclusive recorder | Available | 45 s project lease, 10 s renewal, second-device `409`, generation takeover, old-lease rejection |
| Browser microphone | Available | AudioWorklet, durable server ACK, IndexedDB resend, Chrome and Edge desktop checks |
| Durable audio recovery | Available | Persisted chunks, assembly cursors, 1.5 s minimum speech, 450 ms silence, 5 s cap, tail sealing, and out-of-order recovery |
| Browser tab or shared system audio | Available | `getDisplayMedia` path with a preserved source ID |
| Durable model jobs | Available | SQLite WAL, leases, renewal, retry, idempotent completion, restart recovery |
| Private GPU path | Available | Server queue, private Gateway, DPAPI token, RTX 4080 CUDA Agent |
| Transcript | Available | `faster-whisper:small@cuda` with float16; the real 90-minute network gate measured 1.284 s ASR p95 |
| Translation, explanation, and summary | Available | 3B real-time translation and rolling explanation plus 7B final summary, with completed results required to prove `@cuda` |
| Course material | Available | PPTX, PDF, DOCX, image, Markdown, and text; Qwen3-VL 8B passed a real OCR and visual explanation gate |
| ReadWeave notes | Available | Private ETAPI, 30 s batching, in-page transcript and translation preview, revisions, managed markers, recovery notes, and user-note protection |
| Android | Short device run passed | Foreground service, write-before-send, delete-after-ACK; lock-screen and 90-minute gates remain open |
| DingTalk A1 | Control and recovery path | Public evidence does not establish continuous third-party PCM or incremental transcripts |

## 5. Real model gates

Tested on 2026-08-29 with an RTX 4080 16 GB, public synthetic English lecture audio, small CUDA ASR, and tiered local Ollama models

| Gate | Result |
|---|---:|
| 30-minute full client outage run | 1762/1762 acknowledgements, 485 captions and translations, 90 explanation cards, and one 7B summary; all 41 renewal transport failures recovered with zero gaps, duplicates, fake results, or OOM |
| 30-minute full-outage p95 | ASR 2.336 s, translation 5.319 s, and explanation 7.751 s; every frozen gate passed |
| 90-minute real GPU | 5328 acknowledgements, 1466 captions, 1466 translations, and 278 explanation cards; three outages ended with zero gaps, duplicates, fake results, or OOM |
| 90-minute p95 | ASR 1.284 s, translation 7.886 s, and explanation 10.678 s; every frozen gate passed |
| 30-minute real-model run | 1782 acknowledgements, 490 captions, 490 translations, and 96 explanation cards, with zero gaps, duplicates, or OOM |
| 30-minute provider p95 | ASR 982 ms, translation 6083 ms, and explanation 9316 ms; every frozen gate passed |
| GPU offline recovery | 756 durable chunks produced 208 captions, 208 translations, 17 explanations, and one summary after recovery; all 435 jobs ran once |
| Local image understanding | Qwen3-VL 8B extracted the synthetic slide title and body; cold run 48.743 s and warm run 12.906 s |
| Final summary selection | 14B exceeded the 180 s gate; 7B completed the same evidence-bounded task in 10–12 s and became the default |
| Browser outage recovery | Chrome 5, 15, and 60 s plus Edge 5 s all recovered with zero pending audio |
| Multi-device recording lease | Second device received `409`; takeover after 45 s incremented generation and rejected the old lease |
| Reordered and repeated audio | Sequence 2 before 1 recovered one contiguous window; exact retransmission did not duplicate storage |
| Core restart | A partial tail survived restart and two chunks produced one final caption |
| GPU Agent and private network | Agent restarted automatically; work resumed after a 30 s Gateway outage |
| ReadWeave recovery | Work queued while offline, completed on attempt 4 after recovery, and returned all seven mapped nodes to `synced` |
| Rolling-model fallback | 7B explanation raised translation p95 to 10.272 s; reverting rolling work to 3B reduced the matching short run to 1.919 s, while 7B remains final-summary only |
| Health during 7B summary | A 31.479 s real summary kept the Agent and Worker alive, small CUDA ASR p95 was 1.726 s, and the resident 3B model returned afterward |

These figures describe one synthetic sample and one hardware profile, not every accent, room, or real-course accuracy profile

## 6. Private deployment

The hybrid layout lets the server persist audio while the GPU Agent makes an outbound private connection  The server never initiates a connection to a home public address

```powershell
./scripts/initialize-gpu-agent-secret.ps1 # Generate and protect the Worker token with Windows DPAPI
./scripts/install-gpu-agent.ps1 -GatewayUrl "http://worker-gateway.example.invalid" # Install login startup against a reserved private Gateway example
```

The server stores only a token digest, the Windows user keeps the token under DPAPI, public Nginx rejects `/internal/`, and the Worker Gateway binds only to a private interface

Production Core accepts only the stable Authentik identity overwritten by the trusted reverse proxy  ReadWeave ETAPI remains on the private container network and its token is never sent to a browser

See [deployment](deploy/README.md) for boundaries and rollback behavior

## 7. Validation

```powershell
cargo test --workspace # Run Rust unit and integration tests
cargo clippy --workspace --all-targets -- -D warnings # Treat Rust lints as failures
pnpm lint # Check web code style
pnpm typecheck # Check TypeScript types
pnpm test # Run web tests
pnpm build # Produce the production web build
uv run ruff check workers tools # Check Python code style
uv run mypy workers tools # Check Python types
uv run pytest # Run Python tests
```

Dates, environments, observed timings, and open gates are recorded in [validation](docs/VALIDATION_REPORT.md)

## 8. Data and security

- Recordings, transcripts, course files, tokens, private addresses, and temporary downloads never belong in Git, snapshots, or ordinary logs
- A real session requires a recorded permission confirmation and a continuously visible recording state
- Text and image egress is disabled by default
- User data lives outside immutable releases, so a code rollback does not delete a session
- Repository examples use reserved domains and empty credentials

Use GitHub private security reporting for vulnerabilities  Never attach recordings, tokens, real hostnames, or server details to public issues

## 9. Repository map

| Path | Responsibility |
|---|---|
| `crates/` | Rust state machine, events, durable queue, ingest, and API |
| `workers/` | ASR, translation, explanation, document parsing, and the GPU Agent |
| `apps/web/` | Black-and-white single-page course workspace |
| `apps/android/` | Android long-session capture client |
| `apps/dingtalk-miniapp/` | A1 control and foreground capability probes |
| `deploy/` | Server, Nginx, Authentik, and private Gateway templates |
| `docs/` | Decisions, research, privacy, limitations, changes, and validation |
| `docs/READWEAVE_INTEGRATION.md` | Note layout, projection scope, conflict handling, and recovery |
| `docs/ARCHITECTURE_MENTAL_MODEL.md` | Complete recording, ACK, model, device-sync, and note flow |
| `docs/MODEL_ROUTING.md` | 3B, 7B, VLM, and per-item cloud authorization boundaries |

## 10. Support and license

Use repository issues for defects and feature requests

This repository currently has no `LICENSE` file  Except where applicable law provides otherwise, no permission is granted to copy, distribute, modify, or use the project commercially
