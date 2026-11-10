# pi-subagents 0.80.0 -> 0.87.0 security review

## Scope and verdict

Old upstream: 80f8f79d51ee8075fd39baf7fe8ff36b43c3a790
New upstream: 52e68e9ef0e563d5c97b28519f9999f1b5fbbbea
Local integration: ac816af -> d760714 in swan-lake-common.

All 325 changed upstream paths were assigned exactly once across 18 exhaustive fresh-context review batches using openai-codex/gpt-6.1-sol, three concurrent children. Every reviewer reported complete hunk coverage. Two additional independent reviewers challenged the four upstream findings; a third audited every hunk and full contents of the four local integration files. Parent inspected the findings' source seams and verified assignment completeness. No project/source edits were made.

No evidence of deliberate backdoors, covert exfiltration endpoints, or malicious prompt changes was identified. However, four source-supported upstream security-policy/integrity regressions and one local integration instruction-loss issue prevent an unqualified clean bill of health. These appear accidental; intent cannot be proved from review.

Severity below reflects the parent assessment. High-priority policy-boundary issues are conditional, not proven remote code execution. Configuration fallback is ranked medium because it requires invalid operator configuration rather than an attacker-controlled input crossing a trust boundary.

## Findings

### 1. High priority: completion ownership can be forged across live processes

Source: upstream src/runs/background/result-delivery-ownership.ts:29-45; result-watcher.ts:350,417-425,464,587-610; notify.ts:671-697,948-987.

The new owner predicate accepts the public literal `owning-session` in addition to the current process's owner ID. An incoming literal is returned unchanged, without authenticating a parent-owned launch. Result-supplied dead-process metadata also enables adoption without binding it to a real launch.

A malicious writer with access to the victim's shared results directory and knowledge of its exact session-file identity can submit a uniquely named completion for that session with completionOwnerId=`owning-session`. The victim watcher accepts the fabricated completion and can insert attacker-controlled text and wake the parent. This is conversation/completion integrity loss, not demonstrated command execution or OS-user compromise.

Limits: native file watching discovers the file directly on supported platforms; polling paths may additionally need matching indexes in that shared results directory. A read-only child cannot perform the write. Session matching remains enforced. Old process owner IDs were already exposed in run artifacts, so attacks where the adversary already knows that UUID are pre-existing; the new regression removes the UUID prerequisite for targeting another live process.

Fix: authenticate run/session/owner adoption against parent-controlled launch metadata; never treat serialized sentinel or supplied PID as authority.

### 2. High priority: resumed workflows lose admitted mandatory policy extensions

Source: upstream src/runs/foreground/subagent-executor.ts:5475-5480,5653-5655,5817-5820,6549-6550,8419-8424; src/workflows/workflow-launch-record.ts:17-23; workflow-resume.ts:111-125.

Original workflow admission retains mandatory required-extension snapshots so subsequent children remain constrained even after registration disposal. The new persisted launch record and workflow resume route retain the capability ceiling but not this extension snapshot. Resumption reads the current registry again.

If the original mandatory registration is absent or changed after interruption, a previously pending/fresh child can launch without the original policy extension; otherwise-permitted external runners can escape the mandatory-extension admission rejection. Existing settled/reattached children are not the demonstrated gap.

Limits: normal session-start re-registration of the same policy masks the issue, and automatic resume yields to synchronous startup registration. No model-facing registration disposal exploit was demonstrated. This is a continuation-contract failure under supported host lifecycle changes.

Fix: persist and validate the original admitted mandatory snapshot in the parent-owned launch record and carry it internally through normalization and resumed admission. Fail closed on incompatible runners/missing modules.

### 3. High priority, conditional: new Rex placement ignores denyExtensions

Source: upstream src/runs/shared/herdr-placed-run.ts:39-53,144-147; child-tool-plan.ts:424-443; rex-pane-host.ts:200-212; src/extension/herdr-pi-bridge.ts:147-158.

Ordinary child planning suppresses ambient/configured extensions under a denyExtensions ceiling. New Rex placement serializes a launch without rejecting or transmitting that restriction, starts ordinary Pi, and only later constrains active tools via the bridge. Ambient extension factories/hooks can already have run and observed child content.

Reachable with a working Rex session, fresh native `machine: "local"` child, denyExtensions ceiling and ambient extension, without unsupported bindings/budgets/mandatory-extension contracts. Other guards do not universally reject that configuration.

The underlying Herdr serializer omission predates the bump; new local Rex routing makes it newly reachable. This is extension-loading policy loss, not an OS sandbox escape. Extensions are still trusted host code.

Fix: reject placed launches with denyExtensions before provisioning until the placement protocol can honor it while explicitly loading only trusted bridge runtime.

### 4. Medium priority: invalid config discards autoResume:false and restarts work

Source: upstream src/extension/config.ts:17,193-194,263-277; src/extension/index.ts:1029-1032.

Example syntactically valid configuration:

    { "autoResume": false, "asyncWidgetCollapsed": "false" }

The invalid string-valued widget setting causes whole-config validation failure. Because neither key is in FAIL_CLOSED_CONFIG_KEYS, loadConfig logs an error and returns {}. New UI recovery then interprets autoResume !== false as enabled and can relaunch an eligible interrupted workflow despite the explicit manual-only preference.

Limits: an eligible saved workflow and UI recovery are required; ownership, launch-record, digest, age, claim and attempt checks remain. Any listed fail-closed key in the config causes rejection instead of fallback. The configuration error is logged, so this is not wholly silent. Reviewers ranked this P1; parent ranks it medium based on invalid-configuration preconditions.

Fix: include autoResume in FAIL_CLOSED_CONFIG_KEYS; test valid opt-out plus unrelated invalid field and invalid autoResume itself.

### 5. Local integration, medium/P2: Claude bridge drops child-boundary prompt additions

Local source: lib/pi-subagents-claude-bridge.ts:19-22.
Supporting source: upstream src/runs/shared/subagent-prompt-runtime.ts:227-228,255-263; Pi core extensions/runner.ts:1463-1467; installed pi-claude-bridge 0.11.0 src/index.ts:2208-2255 and src/prompt-capture.ts:324-354.

The new adapter enables bridge-backed native children. Non-fork children receive their child boundary and structured-output guidance through a returned systemPrompt, stored by Pi as forceSystemPrompt. Bridge capture keys on the final rendered prompt, but its projection preserves only portable custom/append/context/skills/sections fields, not that forced addition. A request can therefore match capture and succeed while omitting the child boundary.

This loses instructions distinguishing children from the parent and structured completion guidance; it is not evidence of bypassing enforced tool allowlists or capability ceilings. Applies to newly enabled native non-fork claude-bridge children, not the GPT model used for this review. The bridge projection behavior is pre-existing; the local adapter exposes the new integration route.

Fix: preserve the child boundary in portable bridge prompt sections, or explicitly project force-prompt additions without forwarding Pi's harness wholesale. Assert the actual SDK query system-prompt append in an offline integration test.

## Other conclusions

- Local Nix aliases only re-export original entries; store paths are substituted from trusted derivations.
- Local required bridge registration is session-scoped and fails closed against denyExtensions for ordinary native children. It is not requireForAllRunners, so it is not a mandatory security extension for external/placed runners.
- Dependency changes reviewed: undici 8.10.2 -> 8.12.3; new esbuild 0.28.2 development dependency and platform packages. Changed resolution URLs use npm registry and carry integrity metadata. esbuild has an installation script; local rebuild/prune retain --ignore-scripts, and explicit build execution remains trusted build code.
- Skill/document consolidation did not reveal malicious instructions or concrete authority weakening. Tests predominantly add security hardening coverage.

## Validation limits

This was source/diff review, not an executed penetration test. No builds, test suites, end-to-end exploit probes, dependency tarball/binary audits, generated-bundle verification, or independent hash recomputation were performed. All five findings are source-dataflow-backed, with explicit preconditions. No claim of actual exploitation in this installation is made.

Supporting scratch artifacts (temporary, not committed):
- `/tmp/pi-subagents-security/reviews/`: 18 batch reports.
- `/tmp/pi-subagents-security/validation/`: three follow-up reports.
- `/tmp/pi-subagents-security/manifest.json` and corresponding `*.files` / `*.diff`: exhaustive assignments and diffs.
- `/tmp/pi-subagents-security/upstream`: checkout at the exact new revision.

The findings and evidence summaries above are retained here independently of those temporary artifacts.

Recommendation: patch the relevant boundary/recovery paths or defer the bump; do not treat current 0.87.0 as unconditionally security-clean. Until corrected, avoid using shared writable result storage as a sandbox boundary, avoid relying on denyExtensions for Rex placement, and avoid resuming workflows after changing/removing mandatory policy registrations. Keep config valid; autoResume:false alone is not robust against the identified fallback case.
