# Browser verification: release 7653651

Date: 2026-09-22
Target: `http://127.0.0.1:18787`
Method: Playwright with headless Chromium and an isolated trusted test identity. API health returned 200 and its build/version metadata contained `7653651`.

## Results

| Viewport | Page | Horizontal overflow | Visible controls | Learning tabs | Glossary | Console / page errors |
|---|---:|---|---:|---|---|---|
| 1280×900 | 200 | None; document and body widths were 1280px | 42 | All four selected successfully | 1 grouped region, 4 expandable terms; disclosure opened and explanation was present | 1 console error with a 403 response; 0 uncaught page errors |
| 390×900 | 200 | None; document and body widths were 390px | 29 | All four selected successfully | 1 grouped region, 4 expandable terms; disclosure opened and explanation was present | 1 console error with a 403 response; 0 uncaught page errors |

The verified learning tabs were Current content, Course structure, Course summary, and Course questions. The designated archived project was temporarily unarchived for the session check and confirmed archived again in the `finally` cleanup. No project or session identifiers, course text, screenshots, or credentials were written to this report.

## 403 diagnosis

At both viewports, the failing request was `POST /api/v1/projects/:project_id/sessions/:session_id/topics/ensure`. The server rejected the local tunnel's browser origin under its origin allowlist. The test identity was accepted; this was an origin policy rejection.

The frontend's background caller already catches this request failure, and Playwright recorded no uncaught page errors. No frontend change is safe here: hiding the response would conceal a server security decision, while suppressing the request would skip the topic repair check. Resolving the 403 requires allowing the tunnel origin in server/proxy configuration; no configuration or deployment was changed.

The same production API route was then called on the isolated completed test session with `Origin: https://live.aialra.online` and the trusted test identity. It returned HTTP 200 with no queued, retried, or repaired work. This confirms the production origin is allowed; the local tunnel result remains a test-origin artifact, not a reason to weaken the server allowlist.

Workspace preference PATCH requests were intercepted locally to avoid persisting the test browser's selected project/session, so preference persistence was not verified. The project was rearchived in `finally` and confirmed archived. No screenshots were saved.
