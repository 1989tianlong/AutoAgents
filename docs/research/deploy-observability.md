> 调研笔记原文（英文），2026-11-23。结论已落到部署页与守护的实现里，概述见 [../product-design.md](../product-design.md) 第 5.8 节；这里保留出处与逐条依据。

# Deploy guardianship and agent observability: research notes

Compiled 2026-11-23 for the AutoAgents "Deploy" module. Each claim was read on a cited page unless marked **unverified**. Source keys resolve at the end.

## A. Deployment management feature checklist

### A1. Facts to model directly

**Vercel states.** The `readyState`/`status` enum is `QUEUED`, `INITIALIZING`, `BUILDING`, `READY`, `ERROR`, `CANCELED`, `BLOCKED` [V10]. Related fields: `readySubstate` = `STAGED` (never served production), `ROLLING`, `PROMOTED`; `checksState` = `registered|running|completed`; `checksConclusion` = `succeeded|failed|canceled|skipped`; `errorCode`, `errorMessage`, `errorStep`; timestamps `createdAt`, `buildingAt`, `ready`, `canceledAt`. Builds are canceled at 45 minutes [V8]. `BLOCKED` can mean the commit author is not a team member [V10].

**Cloudflare Pages stages.** Stage `name`: `queued`, `initialize`, `clone_repo`, `build`, `deploy`. Stage `status`: `success`, `idle`, `active`, `failure`, `canceled`, `skipped`. `environment` is `preview` or `production`; `deployment_trigger.type` is `github:push`, `ad_hoc` or `deploy_hook`, with branch and commit metadata [CF9].

**Cloudflare Workers.** A version is an immutable snapshot of code, configuration and bindings; a deployment is the traffic assignment (one version at 100%, or two split) [CF5].

**Netlify.** Status filters: Successful; Unsuccessful (Failed, Canceled, Skipped); Enqueued; Pending review, Accepted or Rejected for unrecognized authors. Deploys are atomic [NL1, NL2]. Per-stage labels **unverified**.

**Render.** Build command (120 minute timeout), optional Pre-deploy command (30 minutes), Start command (15 minutes). On failure the service "continues running its most recent successful deploy" [RD1].

**Railway.** `Crashed` means a non-zero exit after up to 10 automatic restarts [RW1]. Other status labels **unverified**.

**Vercel webhook events** (Pro and Enterprise) [V4, V5]. `deployment.created`; `deployment.ready` (built, checks can start); `deployment.succeeded` (ready, blocking checks passed); `deployment.error`; `deployment.canceled`; `deployment.promoted` (not rollbacks); `deployment.rollback` (accepted only; can arrive "before traffic finishes switching"); `deployment.cleanup`; `project.rolling-release.{started,approved,completed,aborted}`; `alerts.triggered`. Payload keys: `id`, `type`, `createdAt`, `payload`, `region`. `x-vercel-signature` is the HMAC-SHA1 hex digest of the raw body keyed with the webhook secret. Timeout 30 seconds; non-2xx is retried with exponential backoff for up to 24 hours.

**What a Vercel "check" is.** Two features share the name.

- Checks API [V3]: "tests and assertions created and run after every successful deployment". "Vercel waits until all the created checks receive an update. Once all checks receive a `conclusion`, aliases will apply, and the deployment will go live". Flows: Core (200 on pages or APIs), Performance, End-to-end, Optimization.
- Deployment Checks [V6]: "Vercel will hold each production deployment until all required checks pass before assigning it to your custom production domains". Sources: native Lint and Typecheck, GitHub Actions results, Marketplace integrations. `Force Promote` bypasses.

### A2. Prioritised features

M = must-have, N = nice-to-have. V Vercel, CF Cloudflare, NL Netlify, RW Railway, RD Render, GH GitHub Actions.

|M|Feature|Who|Behaviour|
|---|---|---|---|
|M|Deployment list with status, environment, branch/commit, author, duration, URL|all|Vercel filters by branch, date range, environment and status [V9]. Datadog aggregates executions, failures, failure rate, median and p95 duration per service and environment [DD1]|
|M|Preview per PR or branch|V, CF, NL, RD|Vercel deploys every push with branch-specific and commit-specific URLs, linked from PR comments [V7, V11]. Pages gives each branch `<hash>.<project>.pages.dev` plus a branch alias [CF1]|
|M|Build log capture|all|Vercel can fail with no logs (invalid `vercel.json`, non-member author, provisioning failure), so keep `errorMessage` too [V8]|
|M|Redeploy or retry|V, NL, RW|Vercel offers "Use existing Build Cache" [V9]. Netlify retries "from the HEAD of your branch", even if HEAD moved past the original SHA [NL1]. Railway reuses "the exact same code and build/deploy configuration" [RW1]|
|M|Cancel|V, NL, RW|Netlify "Yes, cancel deploy"; Railway "Abort deployment" [NL1, RW1]|
|M|Rollback and promote|V, CF, NL, RW, RD|Vercel re-points domains at a kept build and "takes effect within seconds"; environment variables are not rebuilt [V1, V15]. Pages: "Rollback to this deployment", instant, previews not valid targets [CF2]. Workers: `wrangler rollback` makes a new 100% deployment; "Resources connected to your Worker will not be changed"; blocked by Durable Object lifecycle changes or missing R2, KV or queue bindings [CF4]. Netlify: "Publish Deploy" on an old deploy; "Rollbacks are instantaneous" [NL1]. Railway restores the image and variables [RW1]. Render starts "a new deploy using the target deploy's build artifact" [RD2]|
|M|Stage, then promote|V, CF Workers, NL, GH|Vercel: turn off "Auto-assign Custom Production Domains"; states Staged, Promoted, Current [V2]. Workers: `wrangler versions upload` saves a version without deploying [CF5]|
|M|Production approval gate|GH, V, Argo, Flagger|GH: "up to 6 people or teams", one approval is enough, "Prevent self-review" is opt-in, plus wait timer and branch/tag rules [GH1]. Flagger `confirm-rollout` and `confirm-promotion` block until HTTP 200 [FL3]|
|M|Signed event ingestion|V, GH|Events above. Vercel to GitHub `repository_dispatch` types: `vercel.deployment.{ready,success,error,canceled,ignored,skipped,pending,failed,promoted}` [V7]|
|M|Branch build controls|V, CF, NL, RD|Pages: All non-production, None, or Custom with wildcards; Excludes, then Includes, then Skip [CF3]. Render: `[skip render]`, or auto-deploy "After CI Checks Pass" [RD1]. Vercel cancels superseded queued builds on the same branch [V7]|
|M|Post-deploy health gate|Argo, Flagger, V, Kubernetes|Parameters in A3|
|N|Canary or gradual rollout|V, CF Workers, Argo, Flagger|Vercel: two or more increasing stages, last at 100%; Pro allows one project; `vcrrForceCanary=true` targets the canary [V13]. Workers: split across two versions, the last 100 versions only [CF6]|
|N|Automated analysis and abort|Argo, Flagger|Argo: failed background analysis sets "canary weight back to zero" [AR2]. Flagger: at `threshold`, traffic returns to primary and the canary is scaled to zero [FL2]|
|N|Deployment protection|V, CF, NL|Vercel methods: Vercel Authentication, Passport, Password, Trusted IPs; Standard scope covers everything except production. Agents use `x-vercel-protection-bypass`, `vercel curl` or MCP [V12, V11]. Pages: access policy on previews only [CF1]|
|N|Retention policy|V, CF, NL|Vercel defaults, Hobby and Pro: 30 days for canceled, errored, preview and production; Enterprise: 30 days, 90 days, 180 days, 1 year. Always kept: last 3 (Hobby) or 10 (Pro) deploys, last 3 (Hobby) or 20 (Pro) READY production deploys, aliased deploys, latest deploy of an active-PR branch; 30-day restore window [V14]. Pages: older than 30 days deleted "except the project's 100 most recent and your current production deployment" [CF2]. Workers Builds: logs kept 90 days [CF5]. Netlify "deletes failed and canceled deploys that are 6 months old" [NL2]|
|N|Release and health correlation|Sentry, Datadog|Sentry: a release is "a version of your code deployed to an environment"; `sentry-cli deploys new --release "$VERSION" -e ENVIRONMENT`; crash-free sessions and users; Deploys section lists "when and to which environments this release has been deployed" [SE1, SE3, SE4, SE5]. Datadog CD Visibility is in Preview and lists Rollback Detection and Faulty Deployment Detection [DD1]|
|N|Observability tie-in|V|Observability is on all plans and tracks CDN requests, function invocations, external API requests, middleware invocations and AI Gateway requests [V16]. `alerts.triggered` carries severity and z-score fields [V5]|
|N|Audit and attribution|V|`creator.type` is user, integration, project, system or app; `platform` metadata maps a deployment "back to a platform (e.g. the chat that created it)" [V10]|

### A3. How health-check auto-rollback is normally configured

- **Kubernetes probes** [K1, K2, K3]. `httpGet.path` such as `/healthz`. Success is "Any code greater than or equal to 200 and less than 400". Defaults: `periodSeconds` 10, `timeoutSeconds` 1, `successThreshold` 1, `failureThreshold` 3, so about 30 seconds to declare unhealthy. A `startupProbe` sized as `failureThreshold × periodSeconds` is the grace period (docs example 30 × 10 = 300 seconds). A failed readiness probe removes the pod from Service endpoints. Pod `terminationGracePeriodSeconds` defaults to 30 seconds.
- **Flagger** [FL2, FL3]. `interval` defaults to 60 seconds; example `stepWeight` 2 up to `maxWeight` 50; `threshold` is the failed-check count before rollback, so rollback time = interval × threshold. `progressDeadlineSeconds` defaults to ten minutes. Hooks: "200-202 - advance canary by increasing the traffic weight"; "timeout or non-2xx - halt advancement and increment failed checks".
- **Argo Rollouts** [AR1, AR2]. Steps are `setWeight` plus `pause: {duration}`. Analysis uses `interval`, `count` (0 means until the rollout ends), `successCondition` and `failureCondition` (example `result[0] >= 0.95`), `failureLimit` (default 0). An Inconclusive result pauses for a human.
- **Vercel** [V3, V13]. Blocking checks gate alias assignment; a rolling release advances by stage duration or manual approval; abort is a rollback to the base deployment.
- **KEDA** is an autoscaler only, with no deploy or rollback role [KE1]. **Flux** is **unverified** (docs pages returned 404); search results only say rollback is `git revert` plus reconcile.

Suggested default for the app (synthesis, not a vendor quote): probe `/` and one API path, accept 2xx and 3xx, 10 second interval, 60 to 300 second startup grace, fail after 3 consecutive failures, then keep watching production for 5 to 10 minutes.

## B. Guardian loop design notes

Vercel's own recovery guide is this loop: confirm, roll back, verify, identify, check build logs, compare good against bad, bisect, fix on preview, verify, ship [V15].

1. **Trigger.** Prefer events (`deployment.succeeded`, `deployment.error`, or GitHub `repository_dispatch`) with polling as fallback. Key records by repo, commit SHA and PR number.
2. **Track.** Normalise to QUEUED → BUILDING → READY → CHECKS → PROMOTED, with ERROR, CANCELED and BLOCKED terminal. Show queue, build and deploy durations separately.
3. **Triage build failure.** Store the log tail and `errorMessage`. Route log-less failures to a configuration fix, not a code fix [V8].
4. **Health gate.** Probe the immutable deployment URL, not the production alias. Use the bypass header on protected Vercel previews [V11].
5. **Auto-rollback**, only when a known-good production deployment exists. Model a "rolled back, pinned" state, because Vercel and Netlify stop auto-publishing until the next promote or unlock [V1, NL1].
6. **Repair task.** Include deployment URL, last error with context, good-to-bad diff, probe path and observed status, and the acceptance check. Target a branch.
7. **Fix, preview, same gate.** Netlify ships this shape: "Fix with agent" from a failed deploy, work lands "on a new automated custom branch created off your production branch" with a Deploy Preview [NL4].
8. **Human approval for production.** Required reviewers with no self-approval [GH1]. Vercel Agent: "It never changes production on its own"; "The plan is the permission" [VA1].
9. **Promote, optional canary, record.** Log from and to deployment IDs; Vercel's `deployment.rollback` payload carries `fromDeploymentId` and `toDeploymentId` [V5].

Failure handling to copy:

- **Serialise promote and rollback per project.** Vercel answers a second rollback with `409 Conflict`; a job unfinished after 3 minutes is stale [V1].
- **Deduplicate by event `id`**, reconcile by polling, verify signatures.
- **Rollback is not a fix.** Vercel does not roll back environment variables and warns about "external APIs, databases, and CMSes" [V1]. Render reuses the target's start command and environment but current disks and runtime [RD2].
- **Retention removes rollback targets.** Show an expiry per deployment [V14, CF2].
- **Cap retries.** Railway stops after 10 restarts and marks the service Crashed [RW1].
- **Cancel stale work**, as Vercel does with superseded builds [V7].

## C. Observability and operability checklist

### C1. What Claude Code exports [CC1]

Enable with `CLAUDE_CODE_ENABLE_TELEMETRY=1` plus `OTEL_METRICS_EXPORTER` (`otlp`, `prometheus`, `console`, `none`), `OTEL_LOGS_EXPORTER` and the `OTEL_EXPORTER_OTLP_*` variables. Metrics export every 60 seconds, logs every 5 seconds. Traces are beta (`CLAUDE_CODE_ENHANCED_TELEMETRY_BETA=1`). Message content is off by default and needs `OTEL_LOG_USER_PROMPTS=1` (or `OTEL_LOG_ASSISTANT_RESPONSES`, `OTEL_LOG_TOOL_DETAILS`, `OTEL_LOG_TOOL_CONTENT`). Exporter variables in a repo's `.claude/settings.json` are ignored.

Metrics: `claude_code.token.usage` (input, output, cacheRead, cacheCreation), `claude_code.cost.usage` (USD), `claude_code.session.count`, `claude_code.lines_of_code.count`, `claude_code.pull_request.count`, `claude_code.commit.count`, `claude_code.code_edit_tool.decision`, `claude_code.active_time.total` (user or cli). They are attributed by `model`, `query_source`, `speed`, `effort`, `agent.name`, `skill.name`, `mcp_server.name`, `mcp_tool.name`, and by resource attributes `session.id`, `app.version`, `user.account_id`.

Events: `claude_code.user_prompt`, `claude_code.assistant_response`, `claude_code.tool_result` (`success`, `duration_ms`, `error_type`), `claude_code.tool_decision`, `claude_code.api_request` (`cost_usd`, `request_id`), `claude_code.api_error` (`status_code`, `attempt`). Correlation keys: `prompt.id`, `event.sequence`, `request_id`.

Spans: `claude_code.interaction` → `claude_code.llm_request`, `claude_code.tool` → `claude_code.tool.blocked_on_user`, `claude_code.tool.execution`.

Built-ins: `/usage` shows session tokens and estimated cost; `/insights` writes `~/.claude/usage-data/report.html`. OTel "is the only option that streams per-user token and cost metrics into your own observability stack in near real time" [CC2].

GenAI semantic conventions (moved to `semantic-conventions-genai`, status Development) [OT1]: metrics `gen_ai.client.operation.duration`, `gen_ai.server.request.duration`, `gen_ai.server.time_to_first_token`, `gen_ai.invoke_agent.duration`, `gen_ai.invoke_agent.tool_calls`; token counters `gen_ai.client.inference.usage.{input,output,cache_read.input,cache_write.input,reasoning.output}_tokens`; operations `chat`, `generate_content`, `create_agent`, `invoke_agent`, `execute_tool`.

Back-ends: Langfuse ingests OTLP over HTTP at `/api/public/otel/v1/traces` and maps `gen_ai.usage.*`, `gen_ai.request.model`, `langfuse.session.id`, `langfuse.user.id`, `langfuse.release` [LF1]. LangSmith nests runs (about a span) in a trace of at most 25,000 runs, grouped into threads by `thread_id` [LS1].

### C2. Local files worth reading

|Source|Location and format|Confirmation|
|---|---|---|
|Claude Code transcripts|`~/.claude/projects/<path with "/" replaced by "-">/<uuid>.jsonl`. Record types seen: `user`, `assistant`, `attachment`, `queue-operation`, `api-request`, `tools-used`. Keys include `uuid`, `parentUuid`, `sessionId`, `timestamp`, `cwd`, `gitBranch`, `version`, `isSidechain`. Assistant records carry `message.usage.{input_tokens,output_tokens,cache_creation_input_tokens,cache_read_input_tokens}`|Docs: transcripts are stored "in plaintext under `~/.claude/projects/` for 30 days by default" (`cleanupPeriodDays`) [CC3]. Key layout read on this machine (first 600 lines of the largest transcript, key names only)|
|Codex rollouts|`~/.codex/sessions/YYYY/MM/DD/rollout-<date>-<time>-<uuid>.jsonl`. Record types `session_meta`, `turn_context`, `response_item`, `event_msg`, `token_usage_record`. The `token_count` payload has `info.last_token_usage`, `info.total_token_usage`, `rate_limits.{credits,primary,secondary,plan_type}`; `task_complete` has `duration_ms`, `time_to_first_token_ms`|Docs: `CODEX_HOME` "defaults to `~/.codex`" [CX1]. Layout seen on this machine; `*.zst` archives are also present there (contents not inspected)|
|Many agents|ccusage: `daily`, `weekly`, `monthly`, `session`, and `ccusage <source> <report>`|Roots: Claude Code `~/.claude/`, `~/.config/claude/projects/`; Codex `$CODEX_HOME`; Gemini CLI `~/.gemini/tmp`; Copilot CLI `~/.copilot/session-state/*/events.jsonl` [CU1]|

Transcripts are plaintext conversations. Parse only usage fields, and say so in the UI.

### C3. What to measure

- **Session:** tokens (four types), estimated cost, wall time, turns, tool calls and tool errors, time blocked on approval, outcome (merged, PR opened, abandoned).
- **Agent or model:** cost and tokens by `model`, `effort`, `agent.name`, `mcp_server.name`; API error rate from `api_error.status_code`.
- **Project or repo:** sessions, cost, PRs, commits, deploys attempted, healthy and rolled back, merge-to-healthy time.
- **Machine:** active sessions, CPU and RAM of agent processes (Claude Code does not export these; sample them from the Tauri host), disk and network. `host.name` needs `OTEL_RESOURCE_INCLUDE_HOST=1`.
- **Day:** burn against budget. SRE example for a 99.9% SLO: page at 14.4× burn (1 hour and 5 minute windows), page at 6× (6 hours and 30 minutes), ticket at 1× (3 days and 6 hours) [SW1].

### C4. UIs that matter

1. **Usage dashboard:** daily, weekly, monthly and session views, by model and workspace.
2. **Session and agent list:** status, cost, tokens, branch and PR, with status filters like a deployment list.
3. **Event and audit log:** prompts, tool decisions, deploy created, promoted and rolled back, approvals. Vercel: "Every change records who asked for it, who approved it" [VA1].
4. **Machine metrics:** per-host resources and agent process health.
5. **Notifications:** "Every page should be actionable" and "Every page response should require intelligence" [SB1]; aim for one alert per incident [SB2].
6. **Runbooks:** one per failure class, such as "build failed: missing env var" or "health gate failed: rolled back to X".

Pillars [OP1]: metrics are tokens, cost and errors per session, model and day; traces are prompt → tool → API request; logs are transcripts, build logs and deploy events.

### C5. Peer products

|Product|Usage and cost|Note|
|---|---|---|
|Conductor|Usage page with "chats, estimated cost, and tokens across local workspaces" for the last 7, 30 or 90 days, by model and workspace, under Settings → Usage (v0.98.0, 2026-11-20) [CO1]|v0.96.0 is titled "Know what you're spending" (details not read)|
|Vibe Kanban|None in README|"Vibe Kanban is sunsetting." [VK1]|
|Crystal|None in README|Deprecated February 2026, replaced by Nimbalyst [CR1]|
|Emdash|Site: "track usage and set spend limits across providers" [EM2]; open-source README has no cost tracking [EM1]| |

## D. Surprises and where vendors differ

1. **"Instant rollback" is four mechanisms.** Vercel re-points domains at a kept build. Cloudflare Workers creates a new deployment of an old version. Render creates a new deploy that reuses the artifact. Netlify re-publishes an older atomic deploy.
2. **Rollback pins production** on Vercel and Netlify until a promote or unlock, so the next merge does not go live. Show that state.
3. **Two Vercel features are called "Checks"**, READY does not mean live, and `deployment.rollback` only means accepted.
4. **Default retention is 30 days** on Vercel Hobby and Pro, and Pages keeps 100 deployments plus current production, so rollback history is finite.
5. **Render dashboard rollbacks disable auto-deploy; API rollbacks do not.**
6. **Vercel can fail with no build log**, and its webhook signature is SHA-1 HMAC over the raw body.
7. **Flagger rollback time = interval × threshold.** Kubernetes probe defaults are strict: 1 second timeout, 3 failures at 10 seconds.
8. **Agent vendors converge on "diagnose free, fix costs credits, fix lands on a branch".** Netlify: "Fix with agent" consumes credits [NL4]. v0 merges only on Publish and offers "Fix CI" [VO1]. Vercel Agent is "read-only by default" and "never changes production on its own" [VA1]; its example `sre` agent is investigation-only [VE1].
9. **Deployments already record their agent origin**: Vercel `source: "v0-web"` and `platform` metadata [V10]; Netlify `agent-<run>` preview URLs [NL5].
10. **Cloudflare "C3" is a scaffolding CLI, not an agent.** Agent support is a Skills and MCP plugin (`/plugin install cloudflare@cloudflare`). A Pages project first deployed by Direct Upload cannot later connect to git [CF7, CF8].
11. **For Workers a config change is a new version**, because versions snapshot code and bindings together [CF5].
12. **The Claude Code transcript lines sampled had no cost field**; compute it from `usage`, or read `claude_code.cost.usage` from OTel. Codex rollouts carry `rate_limits`.
13. **Not verified:** Pages retry and config-change behaviour, Flux rollback, Railway status labels, Netlify per-stage labels, and third-party "self-healing deploy" tools (search summaries only). Netlify Split Testing is closed to new projects, Crystal is deprecated and Vibe Kanban is sunsetting [NL3, CR1, VK1].

## Sources

Vercel: [V1] https://vercel.com/docs/instant-rollback
[V2] https://vercel.com/docs/deployments/promoting-a-deployment
[V3] https://vercel.com/docs/checks
[V4] https://vercel.com/docs/webhooks
[V5] https://vercel.com/docs/webhooks/webhooks-api
[V6] https://vercel.com/docs/deployment-checks
[V7] https://vercel.com/docs/git/vercel-for-github
[V8] https://vercel.com/docs/deployments/troubleshoot-a-build
[V9] https://vercel.com/docs/deployments/managing-deployments
[V10] https://vercel.com/docs/rest-api/deployments/get-a-deployment-by-id-or-url
[V11] https://vercel.com/docs/deployments/environments
[V12] https://vercel.com/docs/deployment-protection
[V13] https://vercel.com/docs/rolling-releases
[V14] https://vercel.com/docs/deployment-retention
[V15] https://vercel.com/docs/deployments/rollback-production-deployment
[V16] https://vercel.com/docs/observability
[VA1] https://vercel.com/blog/vercel-agent
[VE1] https://vercel.com/kb/guide/eve-incident-sre-agent
[VO1] https://v0.app/docs/github

Cloudflare: [CF1] https://developers.cloudflare.com/pages/configuration/preview-deployments/
[CF2] https://developers.cloudflare.com/pages/configuration/rollbacks/
[CF3] https://developers.cloudflare.com/pages/configuration/branch-build-controls/
[CF4] https://developers.cloudflare.com/workers/configuration/versions-and-deployments/rollbacks/
[CF5] https://developers.cloudflare.com/workers/configuration/versions-and-deployments/ and https://developers.cloudflare.com/workers/ci-cd/builds/
[CF6] https://developers.cloudflare.com/workers/configuration/versions-and-deployments/gradual-deployments/
[CF7] https://developers.cloudflare.com/pages/get-started/c3/
[CF8] https://developers.cloudflare.com/agent-setup/claude-code
[CF9] https://developers.cloudflare.com/api/resources/pages/subresources/projects/subresources/deployments/methods/list/

Netlify: [NL1] https://docs.netlify.com/deploy/manage-deploys/manage-deploys-overview
[NL2] https://docs.netlify.com/deploy/deploy-overview/
[NL3] https://docs.netlify.com/site-deploys/split-testing/
[NL4] https://docs.netlify.com/build/build-with-ai/agent-runners/overview
[NL5] https://docs.netlify.com/site-deploys/deploy-previews/

Others: [RW1] https://docs.railway.com/guides/deployment-actions
[RD1] https://render.com/docs/deploys
[RD2] https://render.com/docs/rollbacks
[SE1] https://docs.sentry.io/product/releases/
[SE3] https://docs.sentry.io/product/releases/health/
[SE4] https://docs.sentry.io/product/releases/release-details/
[SE5] https://docs.sentry.io/cli/releases/
[DD1] https://docs.datadoghq.com/continuous_delivery/explorer/
[AR1] https://argo-rollouts.readthedocs.io/en/stable/features/canary/
[AR2] https://argo-rollouts.readthedocs.io/en/stable/features/analysis/
[FL2] https://docs.flagger.app/usage/how-it-works
[FL3] https://docs.flagger.app/usage/webhooks
[KE1] https://keda.sh/docs/latest/concepts/scaling-deployments/
[GH1] https://docs.github.com/en/actions/how-tos/deploy/configure-and-manage-deployments/manage-environments
[K1] https://kubernetes.io/docs/tasks/configure-pod-container/configure-liveness-readiness-startup-probes/
[K2] https://kubernetes.io/docs/concepts/workloads/pods/pod-lifecycle/
[K3] https://kubernetes.io/docs/reference/kubernetes-api/workload-resources/pod-v1/

Agent observability: [CC1] https://code.claude.com/docs/en/monitoring-usage
[CC2] https://code.claude.com/docs/en/costs
[CC3] https://code.claude.com/docs/en/data-usage
[CX1] https://learn.chatgpt.com/docs/config-file/config-advanced
[CU1] https://ccusage.com/guide/
[OP1] https://opentelemetry.io/docs/concepts/observability-primer/
[OT1] https://github.com/open-telemetry/semantic-conventions-genai (docs/gen-ai: README.md, gen-ai-spans.md, gen-ai-metrics.md, gen-ai-token-metrics.md)
[LF1] https://langfuse.com/docs/opentelemetry/get-started
[LS1] https://docs.langchain.com/langsmith/observability-concepts
[SB1] https://sre.google/sre-book/monitoring-distributed-systems/
[SB2] https://sre.google/sre-book/being-on-call/
[SW1] https://sre.google/workbook/alerting-on-slos/
[CO1] https://www.conductor.build/changelog/0.98.0-see-where-your-usage-goes
[VK1] https://github.com/BloopAI/vibe-kanban
[CR1] https://github.com/stravu/crystal
[EM1] https://github.com/generalaction/emdash
[EM2] https://emdash.com
