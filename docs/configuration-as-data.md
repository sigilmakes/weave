# Configuration as data

Status: proposed design for #65. Source inspected: `9e7f13530eb449992c7716b13a4dc3552218d95b`.
This document does not add a loader, schema, generator, or CLI command.
New file names, JSON fields, and validation commands below are proposals, not existing APIs.

## Decision and scope

Ship a generated, schema-versioned `config.json`. Generate it from reviewed JSON
source files in the repository. Use sparse user and project `config.json` files
for changes. Load provider declarations and skills from extension directories.
Never put discovered model lists or credential values in configuration.

A provider using an installed transport adapter must be addable with JSON alone.
A new inference protocol still needs an adapter. Configuration is not a language
for implementing streaming, OAuth, request signing, or arbitrary code.

The ticket's description predates some existing behavior. Project settings
already load from `cwd/.prime/agent/settings.json` in
`crates/pa-core/src/settings/storage.rs`. `merge_scopes` in
`settings/merge.rs` overlays them on user settings. Project skills also exist:
`packages/resolve/auto.rs` discovers project resources before user resources.
`skills/loader.rs` has a separate first-wins user-before-project path. This
change must remove that ordering disagreement, not introduce a second loader.
There is no unified `config.json` or provider extension directory today.

The existing `docs/` contains no general configuration or provider design.
`docs/slack-mcp.md` documents an MCP endpoint and its fixed OAuth registration;
that is a tool integration, not an inference provider. The other documents cover
legacy upgrades and daily statistics. The configuration examples in `README.md`,
particularly model overrides and plugin compaction settings, also inform this design.

## Locations and precedence

Keep `.prime/agent` as the directory name. Renaming the product's state directory
is not part of this ticket. Let `A` be the agent directory resolved today by
`pa-cli/src/config.rs::get_agent_dir`: `PRIME_AGENT_CODING_AGENT_DIR` when set,
otherwise `~/.prime/agent`. Let `P` be the session's canonical working directory.
Use exactly that directory, not the Git root or an ancestor search. This matches
the existing project-settings boundary. Report `P` when showing configuration.
A resumed session uses its recorded working directory, not the attaching client's.

| Purpose | Proposed exact location |
| --- | --- |
| Authored distribution defaults | Repository `config/defaults.json` |
| Authored distribution provider records | Repository `config/providers/<provider-id>.json` |
| Authored offline model definitions | Repository `config/models/<provider-id>.json` |
| Schema | Repository `config/schema.v1.json` |
| Generated distribution document | Repository `config.json`; packaged beside the executable |
| User changes | `A/config.json` |
| Project changes | `P/.prime/agent/config.json` |
| User extension providers | `A/extensions/providers/<provider-id>.json` |
| Project extension providers | `P/.prime/agent/extensions/providers/<provider-id>.json` |
| User extension skills | `A/extensions/skills/<skill-name>/SKILL.md` |
| Project extension skills | `P/.prime/agent/extensions/skills/<skill-name>/SKILL.md` |

The packaged document is also embedded as identical JSON bytes for a standalone
binary. It is not fetched from a URL at startup. `PI_PACKAGE_DIR` retains its
existing package-location role; it does not create another user override layer.
An invalid present packaged document is an installation error, not a reason to
silently prefer the embedded copy. The embedded copy serves an absent asset.

Lowest to highest configuration precedence is:

1. Distribution `config.json`.
2. User extension provider records.
3. User `config.json`.
4. Trusted project extension provider records.
5. Trusted project `config.json`.
6. Explicit invocation settings already supported by CLI/session creation.

Process environment is not an extra general configuration layer. Bootstrap and
credential environment reads are exceptions described below. Runtime invocation
settings are never written into an authored layer. Cache and remote discovery
are never steps in this precedence list.

Objects merge by member name. Scalars and arrays replace. Omission inherits.
`null` is invalid except inside `thinkingLevelMap`, where it already means an
unsupported thinking level. In particular, `null` is not deletion. Provider and
skill entries have `enabled: false` to suppress an inherited entry. Map entries
are identified by provider ID or skill name, not by array position. Within one
extension directory each file must have a distinct matching ID; duplicate JSON
keys are errors. Directory iteration order cannot decide a winner.

Provider records are a security exception to recursive merge. Changing `api`,
`baseUrl`, `auth`, or a discovery endpoint replaces that provider's complete
route record. The overriding layer must supply a complete route and credential
reference again. It cannot inherit a stored token or secret header into a new
endpoint. Model metadata and model overrides may still merge by model ID when
the route is unchanged. Report both the winning source and shadowed source.

Relative resource paths resolve against the containing configuration directory;
relative paths in extension skills resolve against that skill's directory.
Provider endpoints must be absolute URLs. Canonical aliases of the user config
are not loaded again as a project layer, preserving today's self-overlay guard.

## Version 1 schema

Use JSON Schema draft 2020-12. `schemaVersion` is the integer `1`, the first
version of this format, not the version of the old model cache. Reject unknown
versions before merging. An incompatible change increments it. Reject duplicate
keys, unknown properties, wrong types, non-finite numbers, JSON comments, and
trailing commas. Do not reuse the lenient settings parser for this format.

Every layer has the following object shape. Only `schemaVersion` is required in
a sparse user or project file. The generated distribution file supplies all
unconditional defaults and complete provider records. `$schema`, if supplied,
is an editor hint, not a network fetch instruction.

| Member | Type and meaning |
| --- | --- |
| `$schema` | Optional string identifying `schema.v1.json` |
| `schemaVersion` | Required integer, exactly `1` |
| `settings` | Optional strict object described below |
| `providers` | Optional map from nonempty provider ID to provider record |
| `skills` | Optional map from skill name to `{enabled: boolean, path?: string}` |
| `catalogs` | Optional user-only object with `models` and `defaultModel` records, each `{url: string, enabled: boolean}` |
| `trustedProjects` | Optional user-only map from canonical absolute project directory to a lowercase SHA-256 digest |

The generated distribution document has `settings`, `providers`, `skills`, and
`catalogs`, but no `trustedProjects`. The latter is a local user's deliberate
approval, not something a repository may approve for itself. An extension provider file is
`{schemaVersion: 1, providers: {<its-file-stem>: <record>}}`; other root members
are forbidden there. Provider IDs retain existing spellings, including
`prime-inference`. No environment-variable name is inferred from an ID.

### Settings

Keep current camelCase setting names and enum spellings. The initial property
set is exactly the serialized fields of `Settings` in
`crates/pa-core/src/settings/types.rs` at the inspected commit, with these changes:

- Remove `onboardingShown`, `onboardingCompleted`, and `recentModels`. They are
  runtime state, not configuration.
- Move `skills` to the root map above. Remove the old string-array form.
- Do not permit `extra` or unknown keys. Plugin-owned `plugins.<id>.config` stays
  an object owned by that plugin; the host validates the declaration, not all
  possible plugin policy fields.
- Keep `plugins` user-only, with the existing `PluginEntry` shape. Reject a
  project declaration instead of ignoring it. Extension folders do not load Lua.
- Make `allowedModels` user-only. Projects may narrow selection through
  `enabledModels`, but cannot widen the daemon's user-level model allowlist.
- For `mcpServers`, use the existing `McpServerConfig` tagged HTTP/stdio shapes,
  not an unchecked map. Secret-valued headers and environment entries use the
  credential reference type below. Literal non-secret entries remain strings.
- `idleEvictionMinutes`, `sessionArchiveMaxAgeDays`, and
  `sessionArchiveMaxSessions` accept nonnegative integers or the existing
  `"off"`/`"none"` spellings. Keep their current zero behavior. Compaction
  percentage accepts finite numbers from 1 through 100, including fractions,
  as enforced in `pa-types/src/compaction.rs`.

Other nested setting objects use their current typed fields. Existing optional
fields remain optional; omission inherits. String settings with a finite set
of valid values use the values accepted by today's getters, including
`treeFilterMode` and `chatDetail`; an invalid value is an error, not a default.
`defaultServiceTier` uses the existing `ServiceTier` vocabulary in `pa-types`.
The implementation must put this complete property set in `schema.v1.json` and
exercise it against the strict Rust reader. The prose does not authorize
silently dropping a field that has a current settings owner.

Defaults move without retuning. For example, idle eviction is 90 minutes,
archive age is 30 days, and archive count is 200 because those are today's
`DEFAULT_*` constants in `settings/manager.rs`. Other values come from that
module's getters, `settings/interactive_settings.rs`, and the retry policy
constructors they call. The generation change must include a source-to-data
inventory for every moved default. Dynamic conditions stay code; their
user-selectable inputs become data. No new polling interval, retry limit,
context size, or output cap is chosen by this design.

### Provider records

Use the existing `ProviderConfig`, `ModelDefinition`, `ModelOverride`,
`ProviderLimits`, and `DiscoveryConfig` vocabulary in
`crates/pa-core/src/models/custom.rs`, with the explicit changes below. These
are JSON records, not executable plugin manifests.

| Member | Version 1 contract |
| --- | --- |
| `enabled` | Optional boolean; distribution records explicitly say `true`; `false` suppresses inference and discovery |
| `name` | Optional display string |
| `api` | Installed transport API ID, required for a complete enabled route |
| `baseUrl` | Absolute HTTP(S) adapter base URL; required except for regional Vertex/Bedrock construction described below; no credentials, query, or fragment |
| `auth` | Required credential selection, using the alternatives defined below |
| `headers` | Optional map of header name to literal non-secret string or credential reference |
| `authHeader` | Optional boolean with the existing request-auth meaning |
| `compat` | Optional object using supported `ModelCompat` fields; unknown flags fail |
| `models` | Optional map from model ID to `ModelDefinition` fields except `id`, `api`, `baseUrl`, and `headers` |
| `modelOverrides` | Optional map from model ID to existing `ModelOverride` fields except `headers` |
| `limits` | Optional object with positive integral `context` and `maxTokens` |
| `discovery` | Optional strict record described below; omission disables live discovery |
| `inferenceParams` | Optional Ollama sampling map, using exactly today's accepted keys and types |
| `modelParams` | Optional map from model ID to the same sampling map |
| `defaultModel` | Optional model ID for this provider |
| `adapterOptions` | Optional strict object selected by `api`, defined below |
| `login` | Optional user-only Prime login record `{apiBaseUrl: string, frontendUrl: string}` |

`apiKey` and `keyless` do not survive into this schema. A credential reference
is exactly one of `{"env": "NAME"}` or `{"store": "provider-id"}`, with a
nonempty string. Provider/discovery `auth` additionally accepts `{"none": true}`,
`{"ambient": "google-vertex"}`, `{"ambient": "amazon-bedrock"}`, or
`{"firstAvailable": [<reference-or-ambient>, ...]}`. The last array is nonempty,
ordered, and cannot nest or contain `none`. Ambient selectors require the matching
adapter; their implementation stays in Rust. No provider ID implies ambient access.
Runtime-supplied credentials remain an explicit invocation override above this list.

Auth has no ENV-or-literal ambiguity. An unset or empty referenced environment
value means unavailable credentials. It never becomes the variable name. `store`
reads only the named stored credential through the auth owner, with its OAuth
refresh behavior, not that ID's ambient environment fallbacks. A stored API-key
entry using the old `!command` form is refused without exposing it; re-login or
an explicit env reference is required. `none` never consults stored or ambient
credentials. No `!command`, interpolation, or literal credential value is accepted.
Distribution chains preserve today's stored-before-env order, except Prime's
existing env-before-stored order; the ordered env aliases come from
`env_api_keys.rs`. A new provider declares its own references instead of editing
that Rust mapping. A secret header uses `{"env": "NAME"}`
or `{"store": "provider-id"}` as a complete value. It is not concatenated by a
configuration expression. Wire-specific Bearer/prefix construction stays in
the adapter. Literal Authorization, Cookie, and API-key headers are refused.
Non-secret header strings are always literal, never interpreted as env names.

Transport API IDs are those registered in `pa-ai/src/registry.rs`:
`openai-completions`, `openai-responses`, `anthropic-messages`,
`azure-openai-responses`, `google-generative-ai`, `google-vertex`,
`mistral-conversations`, `bedrock-converse-stream`, and `openai-codex-responses`.
Reject any other API unless a separately activated transport adapter registers
it through the existing registry. Project data cannot register an adapter.

Static model entries inherit provider routing and must supply `name`,
`reasoning`, `input`, `cost`, `contextWindow`, and `maxTokens` after data defaults
are applied. `input` is a nonempty selection of `text` and `image` containing
`text`; cost has all four existing finite, nonnegative cost fields. Token counts
are positive integers representable by the existing model types. A selected
`maxTokens` remains explicitly configured, preserving `max_tokens_explicit`.
Do not accidentally turn a fallback cap into an explicit user override.
Per-model route or auth selection is removed; a distinct route needs another
provider ID. Thinking maps retain existing level names and null semantics.

`discovery` retains `format`, optional `url`, optional `headers`, and optional
`defaults` from `DiscoveryConfig`; replace its `apiKey` with optional `auth` of
the type above. Formats are today's `openai`, `openrouter`, `anthropic`, `google`,
`codex`, and `ollama`. Prime public/private discovery uses the existing
`CatalogFormat::PrimeInference` code path. The proposed JSON enum additionally
includes `prime-inference` to select that already installed parser; it is not a
new wire protocol. Explicitly trusted local providers may reuse it. Public/private
credential and response filtering remain in that parser, not in arbitrary JSON.
Unknown IDs from formats without adequate metadata require complete explicit
`defaults`, as checked today by `custom.rs::validate_config`. No implicit
32,768/8,192 or 128,000/16,384 model shape is invented for a new provider.
For same-origin discovery, auth may inherit from the route. Cross-origin URLs
require explicit discovery auth, even `{"none": true}`. Credential-bearing
redirects to another origin are refused. Discovery results cannot set routing,
auth, headers, plugins, or configuration.

Ollama sampling keys and their finite-number/string-or-array rules come from
`models/ollama_cloud_config.rs::NUMERIC_INFERENCE_KEYS` and its sanitizers.
`modelParams` replaces that file's `models` sampler map so it cannot be confused
with model definitions. Caps move to ordinary `modelOverrides`. Applying these
sampling maps to an adapter other than the Ollama-supported request path is an
error until that adapter explicitly supports them.

### Skills and trust

A dropped skill directory uses the existing `SKILL.md` frontmatter and optional
Python layout from `skills/discovery.rs`. It is not a plugin. Discovery does not
execute Python or install packages. Extension directories are fixed locations,
not arbitrary recursive code searches. Walk skill directories in sorted path
order, reject cycles and project symlinks escaping the project, and report bad
frontmatter with its path. Existing explicitly configured user paths may still
point outside the agent directory.

Skill winners follow the same scope precedence as providers: project before
user before distribution. A root `skills` entry beats that scope's extension
skill. Two distinct paths declaring the same name within a scope are an error
unless its `skills` map explicitly selects a path. Disabled entries suppress the
name. Continue discovering existing package and `.agents/skills` resources
through `PackageManager`; assign project, user, or distribution scope before
collision resolution. Remove independent first-wins ordering in the skill loader.
Do not download a package just to validate configuration.

A project layer can redirect inference, run MCP processes, select a shell, or
supply Python skills. Parsing it is not approval. Until user approval, report
its files but do not merge any project settings, contribute project skills, run
its commands, install its dependencies, or send discovery requests. Noninteractive
use with an unapproved present project layer fails with an actionable path;
it must not quietly fall back to user configuration.

Record approval only in user `trustedProjects`. The digest covers the canonical
project root, canonical JSON project config/provider records, and relative paths
and bytes of contributed skills and explicit executable resources. Recompute
before activation. Any relevant change requires approval again; absence of a
cache does not revoke or grant approval. Report the project directory, endpoint
origins, credential references, and executable paths before approval, never
credential values. Preserve the existing ban on project Lua declarations even
after approval. Trust is local policy, so it is neither shipped nor committed
with a project. A full sandbox or protection against hostile concurrent file
replacement is outside this ticket; load and activate the approved snapshot,
not paths re-read without checking their content.

### Adapter options and non-inference endpoints

`adapterOptions` is absent for ordinary adapters. For `azure-openai-responses`,
it permits only `azureApiVersion` (nonempty string) and `deploymentNames` (map
from model ID to nonempty deployment name). The version default is `v1`, from
`providers/azure_openai_responses.rs::DEFAULT_AZURE_API_VERSION`. `deploymentNames`
replaces `AZURE_OPENAI_DEPLOYMENT_NAME_MAP`; unmapped IDs use the model ID as
currently. Supply the concrete route `baseUrl` instead of an Azure resource-name
environment variable. Normalize it with the existing adapter code. Per-request
`AzureOpenAIResponsesOptions` continue to override these configured inputs.
Route-changing invocation options require explicit user authority; a project
cannot bypass its route/credential replacement rule with request options.

For `google-vertex`, options permit only `project` and `location`, nonempty
strings, matching `GoogleVertexOptions`. For `bedrock-converse-stream`, they
permit only `region` and `profile`, nonempty strings, matching `BedrockOptions`.
Vertex requires project/location; Bedrock requires region after its existing SDK
region/profile resolution. These two adapters may omit `baseUrl` to request
existing validated regional endpoint construction. An explicitly supplied base
must be a concrete URL, not a template expression. SDK credentials/profile and
region defaults remain SDK-owned; a project can select them only after trust
approval. Other adapter option keys are errors, not arbitrary request properties.
Existing per-request reasoning/tool-choice controls remain request options, not
another configuration layer.

`login` is allowed only for `prime-inference`, with both absolute HTTPS URLs
required when present. Defaults are the existing API/frontend URLs in
`auth/prime_inference.rs`. This replaces its two non-secret endpoint env reads.
Other OAuth providers keep their fixed public client registration and associated
authorize/token endpoints in Rust: those values implement a particular login
protocol, not a generic configurable API-key provider. A new provider does not
automatically gain a browser login. Prime CLI credential reuse must still require
the immutable production URL triple checked today in `auth/prime_inference.rs`;
changing local defaults must not redefine that trust check.

`catalogs.models.url` is the curated model catalog endpoint from
`pa-models/src/fetch.rs::MODEL_CATALOG_URL`. `catalogs.defaultModel.url` is the
selector endpoint from `provider_catalogs.rs::DEFAULT_MODEL_CATALOG_URL`.
They are separate from inference and discovery URLs and use no provider secrets.
Their records require absolute HTTPS URLs and explicit `enabled` booleans in the
distribution data. Projects cannot replace them. Keep refresh scheduling at the
existing `CATALOG_REFRESH_INTERVAL_MS`; this ticket does not change its value.
The plugin/MCP catalog URLs in `fetch.rs` remain with their tool-integration owner;
they are not inference providers or part of this model-configuration schema.

## Generated and authored files

The repository JSON fragments are authored. `config/defaults.json` has the root
layer shape with only `schemaVersion`, `settings`, `skills`, and `catalogs`.
Each `config/providers/<id>.json` has the same single-provider envelope as an
extension record and contains its route/defaults but not its static models.
Each `config/models/<id>.json` has that envelope with only the matching provider's
`models` map. Generation joins these disjoint members and rejects overlapping
source ownership; source filename order never resolves conflicting defaults.
The repository root `config.json`
is generated, committed, and read-only to the runtime. Its header-equivalent
metadata is the schema version; it contains no timestamps, local paths,
credentials, cache results, or machine-derived defaults. Canonical generation
sorts object keys and uses four-space JSON indentation and a final newline.
CI regenerates to a temporary path and byte-compares it. It never repairs the
checked-in file during validation. Intentional changes go in the fragments.
Release packaging includes the schema and the exact checked document. Remote
catalog publication does not regenerate configuration.

User/project files are sparse authored overlays. Initialization generates only
`{"schemaVersion": 1}` at the chosen path, with exclusive creation. It does not
copy all distribution defaults: doing so would freeze them as user choices.
After creation every setting in that file is a user's edit. There is no hidden
machine-owned section in an otherwise editable document. All ordinary mutable
configuration values are editable; the version must remain supported. There is
no generation timestamp or hash that a person must maintain.

Startup never rewrites configuration. Updates replace only the packaged copy,
not overlays. An explicit request to generate a replacement for an existing
user or project file writes a sibling `config.json.next` using exclusive creation
and reports the existing path and proposed path. It fails if that sibling exists.
It never renames it onto `config.json` automatically. The person reviews a diff
and explicitly replaces or merges it. This rule holds even when the existing
file appears unedited; no unreliable edit-detection heuristic is needed.

Interactive settings and model changes remain usable. Intentional persistent
preferences update only their owning user overlay using the existing locked,
atomic read/modify/write discipline. Re-read and validate under the lock; patch
only selected fields; fail without writing on invalid content or conflicting
concurrent edits. Do not persist the merged project/distribution document.
Report when a project override means a saved user preference is not effective.
Recent model use and onboarding flags go to durable runtime state instead.

## What leaves Rust, and what stays

Paths below are relative to `crates/` unless stated otherwise. Function names
refer to inspected code, not proposed interfaces.

| Current source | Proposed treatment |
| --- | --- |
| `pa-ai/src/models_generated.rs`; `pa-models/src/transports.rs::compiled_transport_tuples`; `pa-models/src/pinning.rs::PinnedTemplates` | Move provider URLs, provider/API associations, model definitions, and capability/cost/compatibility data to reviewed JSON. Derive pinning authority from locally shipped or explicitly trusted data, never remote metadata. Retain pin validation and identical offline behavior. AGENTS.md currently requires hand-maintaining this Rust table; implementation must explicitly update that rule, not pretend a generator exists today. |
| `pa-core/src/models/mod.rs::BUILTIN_PROVIDER_IDS`; `models/registry/catalog_sources.rs::builtin_format`, `source_template` | Derive provider membership and discovery selections from validated provider data. Retain format parsers and credential-scope checks. A new ID must not require editing a Rust match or allowlist. |
| `pa-core/src/models/resolver.rs::default_model_per_provider`, `PRIME_INFERENCE_DEFAULT_MODEL_ID`; `pa-core/src/models/registry.rs::xai_subscription_model`, `xai_subscription_thinking_map` | Move named model defaults, subscription endpoint, and per-model maps into provider/model data. Retain selection logic and the OAuth-conditioned choice of the subscription profile. |
| `pa-models/src/fetch.rs::MODEL_CATALOG_URL`; `provider_catalogs.rs::DEFAULT_MODEL_CATALOG_URL`; `ollama_cloud.rs::BASE_URL`; `prime_inference.rs::PRIME_INFERENCE_BASE_URL` | Move distribution endpoints into configuration. Keep payload validators, URL derivation, pagination, native detail fetching, and refresh orchestration. |
| `pa-core/src/settings/manager.rs` getters and default constants; `settings/interactive_settings.rs` | Move policy defaults into `config/defaults.json`; retain validation, setters, locking, and decisions driven by those values. |
| `pa-core/src/models/custom.rs::model_from_definition`; `models/registry/catalog_sources.rs::fact_default_definition` | Remove hidden provider/model default numbers from materialization. Require explicit data defaults or complete model metadata. |
| `pa-models/src/chain.rs::with_bundled_dir` reads `OLLAMA_API_BASE`; `pa-core/src/auth/prime_inference.rs::resolve_prime_inference_auth_config` reads `PRIME_AGENT_INFERENCE_API_BASE_URL` and `PRIME_AGENT_INFERENCE_FRONTEND_URL` | Stop reading endpoint overrides here. Put inference endpoint in the provider route and public login/API/frontend endpoints in a user-only provider login profile. Refuse the obsolete env overrides with a variable-name-only diagnostic. |
| `pa-ai/src/env_api_keys.rs::get_api_key_env_vars` and `get_env_api_key` | Move ID-to-env-name mappings and priority into distribution auth references. Keep environment access in auth resolution. Existing multi-name/provider-specific priority must be recorded explicitly in authored data, not replaced with an ID naming convention. |
| `pa-ai/src/providers/azure_openai_responses.rs` endpoint/version/deployment env reads; `providers/google_vertex.rs` project/location reads | Move non-secret request configuration into the strict adapter options above. Translate Azure resource-name env settings to a concrete `baseUrl`. Keep SDK credential discovery, Bedrock SDK region/profile resolution, and request signing in code. |
| `pa-daemon/src/worker.rs` reads `PRIME_AGENT_MODEL_PROVIDER` and `PRIME_AGENT_MODEL`; `pa-core/src/session_engine/rlm_host.rs` reads `RLM_MAX_DEPTH`; `session_engine/request_timing.rs` reads its timing flag | Use existing settings/explicit invocation inputs, not independent ambient overrides. Obsolete non-secret env values are refused and reported by name with the replacement setting. |
| `pa-cli/src/config.rs`; `pa-types/src/platform/dirs.rs`; worker launch code | Keep agent/session/package/socket bootstrap paths and worker protocol environment in code. They must work before configuration loads and retain explicit-flag precedence. |
| `pa-ai/src/registry.rs`; `pa-ai/src/providers/`; `pa-ai/src/oauth/`; `pa-core/src/auth/` | Keep adapter implementations, wire constants, OAuth protocol/refresh, AWS/Google ambient credential discovery, validation bounds, and secret storage. Prime API/frontend endpoints become the user-only `login` record. Fixed OAuth client registrations and associated endpoints stay with their protocol implementation; they cannot be supplied by a project. |

The strict adapter and Prime login shapes above complete the common route.
They are not hidden in `compat` or accepted as arbitrary JSON.
`PI_CACHE_RETENTION`, AWS transport diagnostic controls, OS/terminal variables,
test hooks, offline/bootstrap variables, and updater controls are not all the
same kind of input. Keep their current behavior unless this table explicitly
moves them. In particular, do not infer that this design removes SDK secrets,
worker coordination, or platform environment support.

## State and deletion

Configuration, discovery data, and secrets have separate roots and authority.
They are not three names for the contents of `settings.json`.

| Kind | Location and lifetime |
| --- | --- |
| Configuration | Repository `config/` plus generated `config.json`; user `A/config.json` and `A/extensions/`; project `.prime/agent/config.json` and `.prime/agent/extensions/`. Safe to commit except local trust policy. Runtime never deletes them. |
| Generated cache/discovery | `A/cache/`, with catalog files under `A/cache/models/`. Existing `provider-model-catalog.v1.json`, `default-model.v1.json`, and scoped discovery snapshots move here. The entire `A/cache/` may be deleted while stopped at any time. It changes startup discovery availability, not credentials, approvals, explicit models, or preferences. |
| Secrets | Existing `A/auth.json`, including MCP OAuth/static-token entries, with existing private permissions/locking; external secret values may come from explicitly referenced environment variables or SDK stores. No project auth file. Never commit, generate into config, print, or copy into cache. |

Sessions, ledgers, plugin saved data, onboarding status, and recent selections
are durable runtime state, not discovery cache. They keep existing session and
plugin locations; the three moved UI state fields live in proposed `A/state/ui.json`.
The cache deletion permission does not apply to these files, the agent directory,
its kernel environment, or secrets. Trust approval is deliberately not cache.
Private discovery snapshots contain metadata only, never token bytes or request
headers; retain credential/account/endpoint scoping and revocation protections in
`pa-models/src/cache.rs`. Recreated cache must not resurrect revoked private models.
No project-local cache is required. The resolved project/config digest participates
in the user cache key so projects do not share incompatible discovery authority.

## Migration without old-file compatibility

This is a format boundary, not an opportunistic merge of every old file.
The new runtime reads only supported `config.json` layers, strict provider
extension records, skill resources, existing secret stores, and durable sessions.
Missing user/project configuration is normal. Malformed or unsupported present
configuration is a blocking error naming the file, JSON pointer, and expected
shape. Do not silently ignore a broken project layer or select another model.

Detect old user/project `settings.json`, `models.json`, and `ollama-cloud.json`.
If any exist, refuse activation and list their paths and replacements. Do not
read their credential-bearing content into an error or automatically import it.
A valid new file does not cause a co-located old file to be ignored: the ambiguity
still blocks. The person translates deliberate preferences and provider data,
uses existing login/auth storage for secret values, then moves the old files
outside the active roots. No backwards-compatibility reader or alias is promised.
Old `extensions` settings, `!command` credentials, JSON-with-comments, and
wrong-typed fields are refused, not repaired by the old `merge::migrate` function.

Existing `auth.json`, MCP grants, sessions, and plugin saved data remain read by
their existing owners. No credential conversion is needed merely to select them
with `auth.store`. Do not delete old catalog caches as a migration side effect;
ignore their old paths and report the new cache location. Legacy model data must
not accidentally become authoritative endpoint data.

Report detected obsolete non-secret environment overrides before startup. List
variable names and replacement pointers, not values. Credential and bootstrap
variables retained by the design are not refused. Initialization and validation
must work without network or login, even when activation is refused. Rollback
means running the prior binary with preserved old files; the new runtime must
not mutate those files. No automatic rollback conversion is promised.

## Validation and acceptance

Proposed local command: `make config-check`, after entering the repository's
existing development environment with `nix develop --no-update-lock-file`.
This target does not exist at the inspected commit. Implementation adds it and
uses exactly the same offline check in `.forgejo/workflows/ci.yml`.
It must use temporary HOME, agent, session, cache, and socket directories and
fake credentials/providers. It must not inspect the maintainer's configuration.

The check must:

1. Validate every source fragment, schema/example fixture, generated distribution
   document, and packaged/embedded copy. Regenerate to a temporary file and fail
   on any byte difference. No timestamps or environment-dependent generation.
2. Check structural schema and semantic rules: installed adapter IDs, route
   completeness, discovery formats, valid model/cost data, credential references,
   extension filenames/IDs, duplicate keys, and unsupported versions. Validate
   each sparse layer before composition and the effective result after it.
3. Prove precedence using whole resolved documents and provenance: nested merge,
   array replacement, disabling, explicit model overrides, skill collisions,
   route replacement without inherited credentials, self-aliasing, and isolated
   projects. Unknown or malformed data must fail without overwriting files.
4. Add one synthetic provider using only a JSON extension file. Use a local fake
   OpenAI-compatible inference/discovery server. It must appear in selection,
   authenticate via a reference, stream a turn, and survive offline use with
   static models, with no provider-specific Rust match or compiled-table edit.
5. Prove generation cannot replace an edited overlay or existing `.next` file;
   concurrent preference writes cannot overwrite unrelated edits. Prove update
   and startup perform no configuration writes.
6. Prove unapproved projects cannot execute or discover anything, changed approved
   content loses approval, and an endpoint override cannot reuse user secrets.
   Capture logs/config/cache and verify fixture secret values never appear.
7. Delete only the isolated cache and show explicit models, credentials, trust,
   preferences, and sessions remain usable. Verify stale discovery publications
   are rejected after route/auth changes. Test a hanging discovery server so
   startup and first paint remain responsive.

Do not replace the repository's full merge gates with this focused check. The
implementation also needs existing CLI/TUI persistence, daemon/worker routing,
provider wire, and TS comparison evidence. The deliberate strict migration,
project trust, and new precedence rules are authorized behavior changes for #65;
record their differences, rather than claiming byte-for-byte parity there.
No build or runtime test is evidence for this design-only document.

## Open questions for review

| Question | Recommendation | Cost of being wrong |
| --- | --- | --- |
| Is a new provider required to implement a genuinely new wire protocol with data alone? | No. Require data-only addition for installed adapters, demonstrated by the acceptance fixture. Keep new protocols as explicit adapter work. | A literal unlimited interpretation would require an unsafe general programming language or leave the ticket impossible to finish. |
| Should arbitrary OAuth registrations be configurable? | No. Move the Prime API/frontend endpoints already configurable today, using `login`; keep fixed OAuth registration/endpoint pairs in their adapter. Azure/Vertex/Bedrock input shapes are defined above. | Some new subscription providers will need login code. Generic OAuth data risks granting credentials to an endpoint the login implementation did not authorize. |
| Should a project be found by Git-root or ancestor search? | Use session cwd only, matching existing settings. Show the resolved path. | Nested invocations can miss root config; changing later requires a new discovery rule and trust review. |
| Should every project-file edit require approval? | Yes for the full contributed configuration/resources snapshot in version 1. Do not weaken it to a directory-name trust bit. | Frequent approvals may be inconvenient. A broad trust bit risks credential exfiltration and executable replacement. |
| Must user overlays be regenerated on every update? | No. Generate a sparse overlay once, then treat it as authored. Only the shipped defaults regenerate routinely. | Users may expect a full editable snapshot; copying defaults would freeze upgrades and make edits impossible to distinguish reliably. |
| Can projects override the user model allowlist or activate Lua? | No. Keep these user-only. | Some projects need explicit user setup. Relaxing this silently would remove current security boundaries. |
| Keep provider facts from Lua as another config layer? | Preserve active plugin-owned provider contributions and their retirement, but reject an ID collision with file-backed providers. Do not give Lua an undocumented precedence slot or auto-load it from extension folders. | Existing colliding plugin/model declarations need manual changes. Retaining today's silent overwrite would make file precedence false and configuration harder to audit. |
| Should missing packaged configuration use the embedded copy? | Yes, with identical bytes. An invalid present asset fails visibly. | This retains standalone binaries but adds a packaging equality check. Silent fallback on corruption would hide broken releases. |

## Implementation order after approval

First approve the adapter/login boundaries and the trust review interaction.
Then add strict schema validation and offline generation with source provenance.
Extract defaults and model/provider data without changing effective values.
Replace settings/model-file composition with one scoped read and preserve existing
crate ownership: `pa-types` for shared vocabulary, `pa-core` for local policy and
auth composition, `pa-models` for discovery/cache, `pa-ai` for transport execution,
and `pa-cli` only for command wiring. Add extension discovery and trust before
allowing project provider activation. Move cache paths and UI state separately.
Finally wire existing settings actions and package validation, update old examples
and the hand-maintained-table rule, and run the focused and existing merge gates.
