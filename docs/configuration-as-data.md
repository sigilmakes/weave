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
otherwise `~/.prime/agent`. Let `P` be the canonical Git worktree root containing
the session's working directory. The working directory triggers discovery; it is not the
configuration boundary. This intentionally changes today's cwd-only settings
boundary. In a directory outside Git, use that canonical working directory as
the project boundary; do not search its ancestors. Report both the working
directory and `P`. A resumed session uses its recorded working directory, not
the attaching client's. Git worktrees use their own root, not the common Git
administrative directory.

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

Ordinary objects merge by member name. Scalars and arrays replace. Omission
inherits. Credential references and every tagged record replace as a whole,
including a change from `{"env": "NAME"}` to `{"store": "provider-id"}`. Never
merge variant members. An MCP `type` change replaces the entire transport
record, so HTTP URL/headers cannot survive in a stdio declaration or vice versa.
`null` is invalid except inside `thinkingLevelMap`, where it already means an
unsupported thinking level. In particular, `null` is not deletion. Provider and
skill entries have `enabled: false` to suppress an inherited entry. Map entries
are identified by provider ID or skill name, not by array position. Within one
extension directory each file must have a distinct matching ID; duplicate JSON
keys are errors. Directory iteration order cannot decide a winner.

Provider routes are a security exception to recursive merge. Routes match by
name, but changing a route's dialect, base URL, path prefix, region binding,
auth selector, discovery endpoint, or destination/account-selecting adapter
options replaces that route as a whole. The latter include `project`, `location`,
`region`, and `profile`. The overriding layer must supply the complete route
and its credential references again; no inherited auth or secret headers survive.
Invalidate resolved routes and in-flight discovery publication guards on any
such change. Other routes under that provider keep their identity. Model metadata
and model overrides may merge by model ID without replacing unchanged routes.
Report both the winning source and shadowed source.

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
unconditional defaults, complete configured provider records, and explicitly
disabled incomplete adapter templates. `$schema`, if supplied,
is an editor hint, not a network fetch instruction.

| Member | Type and meaning |
| --- | --- |
| `$schema` | Optional string identifying `schema.v1.json` |
| `schemaVersion` | Required integer, exactly `1` |
| `settings` | Optional strict object described below |
| `providers` | Optional map from nonempty provider ID to provider record |
| `skills` | Optional map from skill name to `{enabled: boolean, path?: string}` |
| `catalogs` | Optional distribution/user-only object with `models` and `defaultModel` records, each `{url: string, enabled: boolean}` |
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
  not an unchecked map. Transport records and credential references replace
  atomically. As for providers, forbid literal Authorization, Cookie and API-key
  headers, reject URL userinfo, and require references for secret environment
  values. Literal non-secret environment entries remain strings. Plugin config
  and command arguments are explicitly outside any claim that config is safe
  to commit: they can embed secrets the host cannot reliably classify.
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

### Providers and named routes

A provider ID is not a transport API ID. One provider can expose several
protocols and regional endpoints without changing identity. Use existing model,
compatibility, discovery and credential vocabulary, but add named routes rather
than forcing `ProviderConfig`'s single base URL onto the fallback catalog.
These are proposed JSON records, not executable plugin manifests.

| Provider member | Version 1 contract |
| --- | --- |
| `enabled` | Optional boolean request to activate; distribution records specify it explicitly |
| `name` | Optional display string |
| `routes` | Named route list, with unique nonempty `name` members; canonical generated form |
| `route` | Single inline route shorthand; mutually exclusive with `routes` |
| `fallbackOrder` | Ordered list of route names, each exactly once; required with multiple routes |
| `models` | Map from model ID to model metadata and a required `route` name |
| `modelOverrides` | Map from model ID to existing `ModelOverride` metadata fields except `headers` |
| `defaultModel` | Optional model ID within this provider |
| `login` | Optional user-only Prime login record `{apiBaseUrl: string, frontendUrl: string}` |

A model has no independent provider, dialect, base URL or auth selector inside
this record. Its enclosing map supplies provider identity and its `route` names
a route under that same provider. A single-route authored provider may put that
route in `route`, omit its name, and omit model `route` fields.

Normalize every authored layer independently before cross-layer composition.
This includes distribution source and generated distribution records, user and
project files, and user/project extension records at runtime. Normalization is
not just a generation step. Reject a layer declaring both `route` and `routes`.
Expand its shorthand into a named route list, then remove the `route` member;
composition only ever sees canonical `routes`.

Use the shorthand's explicit route name when supplied, otherwise `default`.
Bind only models declared in that same layer with an omitted `route` to that
name. Do not rewrite inherited model bindings. When the layer omits
`fallbackOrder`, emit a singleton list for its shorthand route; an explicit
`fallbackOrder` is retained. `default` is a schema convention, not another
provider identity. Multiple declared routes never infer a model's binding.

Canonical route lists merge across layers by name, not array index. Thus shipped
`routes: [{name: "default", ...}]` plus user `route: {...}` replaces the same
named route and leaves neither an inline member nor a duplicate route. Normal
route-identity and credential-replacement rules still apply. Shorthand without
a name never guesses a differently named inherited route: it declares `default`.
To override an inherited route with another name, name it explicitly. If other
inherited routes remain, the replacing `fallbackOrder` must list the complete
effective set; otherwise reject composition with a diagnostic naming the missing
route names and asking for an explicit order. Check model references and order
coverage after composition, not against an isolated sparse layer.

| Route member | Version 1 contract |
| --- | --- |
| `name` | Nonempty route identifier, unique within the provider |
| `dialect` | Installed transport API ID; the existing runtime model's `api` is derived from it |
| `baseUrl` | Absolute HTTP(S) base URL without userinfo, query or fragment; may contain the single supported `{region}` host placeholder |
| `pathPrefix` | Optional absolute path prefix appended to the base path before the adapter's protocol suffix; no query, fragment, or parent traversal |
| `region` | Optional nonempty binding for `{region}`; a model-specific regional route supplies its binding here |
| `auth` | Credential selection defined below; no implicit provider-ID lookup |
| `headers` | Map of header name to literal non-secret string or credential reference |
| `authHeader` | Optional boolean with existing request-auth meaning |
| `compat` | Supported `ModelCompat` fields for this route's dialect; unknown flags fail |
| `limits` | Optional positive integral `context` and `maxTokens` |
| `discovery` | Optional strict record described below; omission disables live discovery |
| `adapterOptions` | Optional strict dialect-specific object defined below |
| `inferenceParams` | Optional Ollama sampling map using today's accepted keys and types |
| `modelParams` | Optional map from model ID to the same sampling map |

Base path and prefix join at one slash. Adapters still append their existing
protocol suffix: `/v1/messages` for Anthropic, `/responses` for OpenAI Responses,
and `/chat/completions` for OpenAI Completions. Do not normalize an Anthropic
route into an OpenAI route. Only the reserved `{region}` host placeholder is
resolved by the route loader. It accepts a validated region/location label,
never arbitrary URL text. Other placeholders already present in shipped model
paths, such as Cloudflare account/gateway slots, remain inert template text
until an authored complete path replaces them. There is no implicit environment
interpolation based on their spelling.

#### Installed, ready, and enabled

The strict schema permits incomplete adapter templates and computes readiness
separately from the requested `enabled` flag. Ship these templates with
`enabled: false`. Route destination fields, auth and adapter options may be
omitted, and shipped account/gateway/location placeholders may remain unresolved.
Setting `enabled: true` does not make missing fields structurally valid routes:
the provider remains unready and inactive, with a report rather than a fatal
startup error.
A requested enabled provider is effective only when at least one complete route
exists in an authored layer. Authored includes reviewed distribution source data;
a remote catalog is never an authored layer. An incomplete installed template is
unready, not a fatal configuration error. Missing account, gateway, location or
region fields leave it unready. Report the provider, missing fields, and source,
then continue startup with other ready providers. An invocation selecting it
fails with the same readiness report, without falling back to another provider.

Enabling a template requires an authored complete route, explicit credential
selection, and model bindings. A malformed value, unknown dialect, bad reference
or contradictory tagged record is still a strict validation error, not an
unready provider. Readiness does not require the actual secret to be available
while validating files: missing secret values mean unauthenticated, not malformed.
CI runs the same pure readiness rule and compares the expected ready/unready
status of every distribution fixture. It must accept disabled templates and must
not silently certify them as usable routes. Discovery cannot complete or enable
a template. A provider can retain unready templates alongside ready routes; only complete
routes participate in selection and discovery.

#### Route and catalog fallback

`fallbackOrder` is explicit provider data, not JSON/file iteration order. It
orders eligible routes when a provider-wide operation needs a route but no
exact model binding is available, and orders fallback model selection after an
explicit preferred/default model cannot be used. Within a route, candidate
model IDs use the existing lexical order. Exact model selection always uses
that model's named route. A route serving a different model or dialect is not
an automatic inference retry target. Cross-provider turn failover remains the
existing separately configured resilience policy; this design does not invent
same-provider cross-dialect replay. An empty eligible set reports unavailable.

Today `models_generated.rs::get_models` sorts by model ID and
`models/registry/catalog_sources.rs::source_template` selects the first model;
it does not implement a network retry through every provider tuple. Preserve
that first-template choice when extracting `fallbackOrder`: order each route by
the first model ID currently using it. Cloudflare therefore orders Anthropic
before OpenAI; the four Bedrock entries order US, AU, EU, JP. Making later route
choices explicit removes the hidden dependence on a representative model.
Do not claim that a successful request is retried today in this order.

The independent catalog-source fallback in `pa-models/src/chain.rs::baseline`
also remains explicit: validated last-good curated cache, then packaged bundled
snapshot, then the shipped offline definitions replacing compiled Rust. Trusted
local seeds overlay that baseline, then scoped discovery metadata is projected.
An unavailable remote route never deletes an offline model's local binding.
Generated data must reproduce all current `(provider, model ID, api, baseUrl)`
tuples before runtime endpoint overrides, not merely one tuple per provider.

#### Credentials, discovery, and model metadata

`apiKey` and `keyless` do not survive into this schema. A credential reference
is exactly `{"env": "NAME"}` or `{"store": "provider-id"}`, with a nonempty
string. Route/discovery `auth` additionally accepts `{"none": true}`,
`{"ambient": "google-vertex"}`, `{"ambient": "amazon-bedrock"}`, or
`{"firstAvailable": [<reference-or-ambient>, ...]}`. The last array is nonempty,
ordered, and cannot nest or contain `none`. These are tagged alternatives:
replacement cannot retain another variant's members. Ambient selectors require
the matching adapter. No provider ID alone implies ambient access.
Runtime-supplied credentials remain explicit invocation overrides.

Auth has no ENV-or-literal ambiguity. An unset/empty referenced environment
value means missing credentials, never the variable name. `store` reads only
the named stored credential through its auth owner, including OAuth refresh,
not that ID's ambient fallbacks. A stored API-key entry using old `!command`
syntax is refused without exposing it; re-login or an explicit env reference is
required. `none` never consults stored/ambient credentials. No command execution,
interpolation or literal credential value is accepted. Distribution chains
preserve stored-before-env ordering except Prime's env-before-stored ordering;
ordered aliases come from `pa-ai/src/env_api_keys.rs::get_api_key_env_vars`.
A new provider declares references in JSON instead of editing that Rust map.

Secret headers use a complete env/store reference; wire prefix construction
stays with the adapter. Forbid literal Authorization, Cookie and API-key headers.
Non-secret header strings are literal, not env names. Reference resolution and
route identity must cover every credential-bearing header, not just Authorization.

Dialects are the existing API IDs registered in `pa-ai/src/registry.rs`:
`openai-completions`, `openai-responses`, `anthropic-messages`,
`azure-openai-responses`, `google-generative-ai`, `google-vertex`,
`mistral-conversations`, `bedrock-converse-stream`, and `openai-codex-responses`.
Unknown dialects fail unless an explicitly activated adapter registers that
existing API identifier. Project data cannot register an adapter.

Static models supply `name`, `reasoning`, `input`, `cost`, `contextWindow` and
`maxTokens` after authored defaults apply. Input is a nonempty subset of text/image
containing text. Costs have all four existing finite nonnegative fields. Token
counts are positive integers representable by the existing model types.
Thinking maps retain the existing level names and null semantics.

Shipped, bundled, cached and newly discovered `maxTokens` metadata is never an
explicit request preference. Its materialized model has `max_tokens_explicit:
false`. Only a write of `maxTokens` in an authored user/project model definition
or override, including an extension record in that scope, sets it true, with
that source recorded. Copying shipped metadata
into a generated or cached record is not selecting it. Generated distribution
JSON does not serialize this internal provenance flag. Keep the 32,000 request
cap in `pa-ai/src/providers/simple_options.rs::default_request_max_tokens`
exactly: non-explicit metadata is capped; a user's explicit value is not silently
clamped to that cap. Do not change provider-specific thinking-budget handling.

`discovery` has `format`, optional `url`, optional `headers`, optional `auth` and
optional `defaults`. Formats are `openai`, `openrouter`, `anthropic`, `google`,
`codex`, `ollama`, plus proposed `prime-inference` selecting the already installed
`CatalogFormat::PrimeInference`. Prime's public/private authorization filtering
stays code. OpenAI, Anthropic, Google and Codex discovery are metadata-poor:
unknown IDs require explicit complete local defaults for context/output limits,
reasoning, text/image input and all four finite nonnegative cost rates. OpenRouter,
Ollama and Prime discovery may supply complete provider metadata instead; missing
cost rates are not zero. Without complete metadata or defaults, drop the unknown
ID with a credential-free warning naming provider/route/model, not a fatal refresh
or startup error. Preserve already known local models on incomplete updates.
No implicit 32,768/8,192 or 128,000/16,384 shape is invented for new IDs.

Same-origin discovery may inherit route auth and headers. Cross-origin discovery
inherits neither headers nor credentials: it must explicitly declare its own
`auth` and `headers` (possibly `none` and an empty map). Refuse cross-origin
redirects carrying any credential-bearing header, including API keys and cookies.
Discovery payloads cannot set routing, credentials, headers, plugins or config.

Ollama's accepted sampler keys/types come from
`models/ollama_cloud_config.rs::NUMERIC_INFERENCE_KEYS` and its sanitizers.
`modelParams` replaces its old `models` sampler map; caps use model overrides.
A sampler map on another unsupported adapter is a validation error.

### Adapter options and non-inference endpoints

Route `adapterOptions` is absent for ordinary dialects. Azure permits only
`azureApiVersion` and `deploymentNames` (model-ID to nonempty deployment-name map).
The version default is `v1` from
`providers/azure_openai_responses.rs::DEFAULT_AZURE_API_VERSION`. Unmapped models
use their IDs. Translate Azure resource-name env settings into a complete route
base URL rather than adding a second ambient endpoint override. Existing explicit
per-request `AzureOpenAIResponsesOptions` still override configured inputs under
invocation authority; projects cannot bypass route replacement with such options.

Vertex permits only nonempty `project` and `location`, matching
`GoogleVertexOptions`. Bedrock permits only nonempty `region` and `profile`,
matching `BedrockOptions`. Region can be supplied by the route binding;
contradictory `region`/`location` bindings and adapter options are errors. Existing
explicit request options and environment/profile resolution still precede a
Bedrock model's standard regional fallback exactly as in
`providers/bedrock/auth.rs::resolve_endpoint`. A custom fixed gateway base retains
its explicit-endpoint behavior. Do not make a route's fallback region override an
existing explicit region or profile selection.

These options are part of route identity. Resolve the complete endpoint,
project/account/location/region/profile selector and credential references before
project approval and show them together. A change invalidates inherited auth,
secret headers, cached route resolution and previous approval. Ambient profile
resolution cannot hide a different effective account or region behind an unchanged
JSON path; if that effective selection changes, require a new approval.

Bedrock credential and profile-region lookup is hand-written code in
`pa-ai/src/providers/bedrock/auth.rs`, not SDK discovery. Vertex obtains a token
from `GOOGLE_OAUTH_ACCESS_TOKEN` or `gcloud auth print-access-token` in
`providers/google_vertex.rs::resolve_adc_access_token`, not an SDK credential
chain. Preserve both behaviors. Replacing them with an SDK requires a separate
decision. Resolve non-secret profile/account selectors for approval without
printing or persisting token values.

`login` applies only to `prime-inference`, with both absolute HTTPS URLs required
when present. Defaults come from `auth/prime_inference.rs`; it replaces that
module's two endpoint environment reads. Fixed OAuth client registrations and
authorize/token endpoint pairs stay in Rust. A new API-key provider does not
automatically gain browser login. Prime CLI credential reuse still requires the
immutable production URL triple checked today; local defaults cannot redefine it.

The xAI subscription profile stays code in version 1:
`pa-core/src/models/registry.rs::xai_subscription_model` conditionally changes
the API, endpoint and compatibility after authentication, and
`xai_subscription_thinking_map` contains named model mappings plus an all-null
unknown-model fallback. The strict static field table cannot express that
conditional transformation and fallback. Do not move it partly into provider
JSON and claim the old behavior is representable. A later ticket may design
that data shape; its existing code constants remain authoritative for now.

`catalogs` exists only in distribution and user layers. Strict readers reject it
in projects and extension records, including disabled ones.
`catalogs.models.url` comes from `pa-models/src/fetch.rs::MODEL_CATALOG_URL`;
`catalogs.defaultModel.url` comes from
`provider_catalogs.rs::DEFAULT_MODEL_CATALOG_URL`. Both require HTTPS URL and
explicit enabled boolean in distribution data; neither carries provider secrets.
Keep the existing `CATALOG_REFRESH_INTERVAL_MS`. Plugin/MCP catalog URLs remain
with their tool-integration owner, not this inference configuration schema.

### Existing multi-route providers represented

At this base the file is `crates/pa-ai/src/models_generated.rs`, not a file under
`pa-models`; `pa-models/src/transports.rs` consumes it. The following are full
provider records with the current fallback models' metadata, not new model
defaults. Route names are local identifiers chosen here. Model IDs, dialects,
URLs, costs, thinking maps and limits come from the existing `RawModel` entries.
These are distribution-data examples, so their `maxTokens` is not explicit.

Cloudflare keeps the single ID `cloudflare-ai-gateway`. Anthropic and OpenAI
models select different routes under it. Joining each base and prefix reproduces
the table's two original base URLs exactly. Account/gateway slots are unready
installation templates, not secrets, env reads or fabricated account defaults.
A user completes both prefixes and enables the provider; until then startup
reports it unready and retains its offline definitions without attempting a call.

```json
{
    "schemaVersion": 1,
    "providers": {
        "cloudflare-ai-gateway": {
            "enabled": false,
            "routes": [
                {
                    "name": "anthropic",
                    "dialect": "anthropic-messages",
                    "baseUrl": "https://gateway.ai.cloudflare.com",
                    "pathPrefix": "/v1/{CLOUDFLARE_ACCOUNT_ID}/{CLOUDFLARE_GATEWAY_ID}/anthropic",
                    "auth": {
                        "firstAvailable": [
                            {
                                "store": "cloudflare-ai-gateway"
                            },
                            {
                                "env": "CLOUDFLARE_API_KEY"
                            }
                        ]
                    }
                },
                {
                    "name": "openai",
                    "dialect": "openai-responses",
                    "baseUrl": "https://gateway.ai.cloudflare.com",
                    "pathPrefix": "/v1/{CLOUDFLARE_ACCOUNT_ID}/{CLOUDFLARE_GATEWAY_ID}/openai",
                    "auth": {
                        "firstAvailable": [
                            {
                                "store": "cloudflare-ai-gateway"
                            },
                            {
                                "env": "CLOUDFLARE_API_KEY"
                            }
                        ]
                    }
                }
            ],
            "fallbackOrder": [
                "anthropic",
                "openai"
            ],
            "models": {
                "claude-fable-5": {
                    "route": "anthropic",
                    "name": "Claude Fable 5",
                    "reasoning": true,
                    "thinkingLevelMap": {
                        "off": null,
                        "xhigh": "xhigh",
                        "max": "max"
                    },
                    "input": [
                        "text",
                        "image"
                    ],
                    "cost": {
                        "input": 10.0,
                        "output": 50.0,
                        "cacheRead": 1.0,
                        "cacheWrite": 12.5
                    },
                    "contextWindow": 1000000,
                    "maxTokens": 128000
                },
                "gpt-4.1": {
                    "route": "openai",
                    "name": "GPT-4.1",
                    "reasoning": false,
                    "input": [
                        "text",
                        "image"
                    ],
                    "cost": {
                        "input": 2.0,
                        "output": 8.0,
                        "cacheRead": 0.5,
                        "cacheWrite": 0.0
                    },
                    "contextWindow": 1047576,
                    "maxTokens": 32768
                }
            }
        }
    }
}
```

Bedrock keeps the single ID `amazon-bedrock`. Each model binds to its existing
regional endpoint. Expanding the four region bindings yields respectively
`https://bedrock-runtime.us-east-1.amazonaws.com`,
`https://bedrock-runtime.ap-southeast-2.amazonaws.com`,
`https://bedrock-runtime.eu-central-1.amazonaws.com`, and
`https://bedrock-runtime.ap-northeast-1.amazonaws.com`. Those regions come from
the existing four entries, not a new geographic default. The authored route
provides a fallback endpoint; explicit request/environment/profile precedence
in `resolve_endpoint` still applies. Its ambient selector requests the existing
hand-written auth path and does not imply that credentials exist on this host.

```json
{
    "schemaVersion": 1,
    "providers": {
        "amazon-bedrock": {
            "enabled": true,
            "routes": [
                {
                    "name": "us",
                    "dialect": "bedrock-converse-stream",
                    "baseUrl": "https://bedrock-runtime.{region}.amazonaws.com",
                    "region": "us-east-1",
                    "auth": {
                        "ambient": "amazon-bedrock"
                    }
                },
                {
                    "name": "au",
                    "dialect": "bedrock-converse-stream",
                    "baseUrl": "https://bedrock-runtime.{region}.amazonaws.com",
                    "region": "ap-southeast-2",
                    "auth": {
                        "ambient": "amazon-bedrock"
                    }
                },
                {
                    "name": "eu",
                    "dialect": "bedrock-converse-stream",
                    "baseUrl": "https://bedrock-runtime.{region}.amazonaws.com",
                    "region": "eu-central-1",
                    "auth": {
                        "ambient": "amazon-bedrock"
                    }
                },
                {
                    "name": "jp",
                    "dialect": "bedrock-converse-stream",
                    "baseUrl": "https://bedrock-runtime.{region}.amazonaws.com",
                    "region": "ap-northeast-1",
                    "auth": {
                        "ambient": "amazon-bedrock"
                    }
                }
            ],
            "fallbackOrder": [
                "us",
                "au",
                "eu",
                "jp"
            ],
            "models": {
                "amazon.nova-2-lite-v1:0": {
                    "route": "us",
                    "name": "Nova 2 Lite",
                    "reasoning": true,
                    "input": [
                        "text",
                        "image"
                    ],
                    "cost": {
                        "input": 0.33,
                        "output": 2.75,
                        "cacheRead": 0.0,
                        "cacheWrite": 0.0
                    },
                    "contextWindow": 128000,
                    "maxTokens": 4096
                },
                "au.anthropic.claude-haiku-4-5-20251001-v1:0": {
                    "route": "au",
                    "name": "Claude Haiku 4.5 (AU)",
                    "reasoning": true,
                    "input": [
                        "text",
                        "image"
                    ],
                    "cost": {
                        "input": 1.0,
                        "output": 5.0,
                        "cacheRead": 0.1,
                        "cacheWrite": 1.25
                    },
                    "contextWindow": 200000,
                    "maxTokens": 64000
                },
                "eu.anthropic.claude-fable-5": {
                    "route": "eu",
                    "name": "Claude Fable 5 (EU)",
                    "reasoning": true,
                    "input": [
                        "text",
                        "image"
                    ],
                    "cost": {
                        "input": 11.0,
                        "output": 55.0,
                        "cacheRead": 1.1,
                        "cacheWrite": 13.75
                    },
                    "contextWindow": 1000000,
                    "maxTokens": 128000,
                    "thinkingLevelMap": {
                        "off": null,
                        "xhigh": "xhigh",
                        "max": "max"
                    }
                },
                "jp.anthropic.claude-haiku-4-5-20251001-v1:0": {
                    "route": "jp",
                    "name": "Claude Haiku 4.5 (JP)",
                    "reasoning": true,
                    "input": [
                        "text",
                        "image"
                    ],
                    "cost": {
                        "input": 1.0,
                        "output": 5.0,
                        "cacheRead": 0.1,
                        "cacheWrite": 1.25
                    },
                    "contextWindow": 200000,
                    "maxTokens": 64000
                }
            }
        }
    }
}
```

Extraction must round-trip these examples to the same six original
`(provider, model ID, api, baseUrl)` tuples, with unchanged metadata and
`max_tokens_explicit: false`. The test also covers another current multi-dialect
provider, `github-copilot`, rather than assuming only Cloudflare needs routes.
A separate whole-object precedence fixture, `single-route-shorthand-overlay`,
loads a canonical shipped provider with one `default` route and a user overlay
using inline `route`. Compare the complete composed provider and provenance
against the explicit canonical expected object: exactly one replaced `default`
route, no retained `route` member, no duplicate route members, unchanged inherited
model bindings, and the expected singleton fallback order. Also test an explicitly
named shorthand and reject incomplete fallback coverage of inherited routes.

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
name. Discover project package and `.agents/skills` resources only between the
session working directory and its Git root, inclusive. Record canonical paths,
assign project scope, and require approval before contribution. Stop at the Git
root; nothing above it enters through an ancestor scan. Existing explicitly
configured user resources remain user scope, not implicit project ancestors.
Remove independent first-wins ordering in the skill loader. Do not download a
package just to validate configuration.

A project layer can redirect inference, run MCP processes, select a shell, or
supply Python skills. Parsing it is not approval. Until user approval, report
its files but do not merge any project settings, contribute project skills, run
its commands, install its dependencies, or send discovery requests. Noninteractive
use with an unapproved present project layer or project resource fails with an
actionable canonical path; it must not quietly fall back to user configuration.
A skill-only project triggers exactly the same gate even without `config.json`.
This includes project `.agents/skills`, extension skills, and package resources,
not only resources named by the new configuration file.

Record approval only in user `trustedProjects`. The digest covers the canonical
Git worktree root, canonical JSON project config/provider records, canonical
paths and bytes of all contributed resources, and the fully resolved route
endpoint/account selectors described above. Recompute before activation.
Any relevant content, canonical-path, endpoint or account-selection change
requires approval again; cache absence neither revokes nor grants approval.
Show the complete resolved endpoint and non-secret account/project/location/
region/profile selectors, credential references and executable canonical paths
before approval. Never show credential values. If an effective selector cannot
be established, do not approve a hidden account under a generic route label. Preserve the existing ban on project Lua declarations even
after approval. Trust is local policy, so it is neither shipped nor committed
with a project. A full sandbox or protection against hostile concurrent file
replacement is outside this ticket; load and activate the approved snapshot,
not paths re-read without checking their content.

## Generated and authored files

The repository JSON fragments are authored. `config/defaults.json` has the root
layer shape with only `schemaVersion`, `settings`, `skills`, and `catalogs`.
Each `config/providers/<id>.json` has the same single-provider envelope as an
extension record and contains its named routes, fallback order and defaults but
not its static models.
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
| `pa-core/src/models/resolver.rs::default_model_per_provider`, `PRIME_INFERENCE_DEFAULT_MODEL_ID` | Move named provider defaults into provider data; retain selection logic. |
| `pa-core/src/models/registry.rs::xai_subscription_model`, `xai_subscription_thinking_map` | Keep the subscription endpoint constant, named thinking maps, conditional transformation and all-null unknown-model fallback in Rust in version 1. A later ticket must design a data representation before moving them. |
| `pa-models/src/fetch.rs::MODEL_CATALOG_URL`; `provider_catalogs.rs::DEFAULT_MODEL_CATALOG_URL`; `ollama_cloud.rs::BASE_URL`; `prime_inference.rs::PRIME_INFERENCE_BASE_URL` | Move distribution endpoints into configuration. Keep payload validators, URL derivation, pagination, native detail fetching, and refresh orchestration. |
| `pa-core/src/settings/manager.rs` getters and default constants; `settings/interactive_settings.rs` | Move policy defaults into `config/defaults.json`; retain validation, setters, locking, and decisions driven by those values. |
| `pa-core/src/models/custom.rs::model_from_definition`; `models/registry/catalog_sources.rs::fact_default_definition` | Remove hidden provider/model default numbers from materialization. Require explicit data defaults or complete model metadata. |
| `pa-models/src/chain.rs::with_bundled_dir` reads `OLLAMA_API_BASE`; `pa-core/src/auth/prime_inference.rs::resolve_prime_inference_auth_config` reads `PRIME_AGENT_INFERENCE_API_BASE_URL` and `PRIME_AGENT_INFERENCE_FRONTEND_URL` | Stop reading endpoint overrides here. Put inference endpoint in the provider route and public login/API/frontend endpoints in a user-only provider login profile. Refuse the obsolete env overrides with a variable-name-only diagnostic. |
| `pa-ai/src/env_api_keys.rs::get_api_key_env_vars` and `get_env_api_key` | Move ID-to-env-name mappings and priority into distribution auth references. Keep environment access in auth resolution. Existing multi-name/provider-specific priority must be recorded explicitly in authored data, not replaced with an ID naming convention. |
| `pa-ai/src/providers/azure_openai_responses.rs` endpoint/version/deployment env reads; `providers/google_vertex.rs` project/location reads | Move non-secret request configuration into the strict adapter options above. Translate Azure resource-name env settings to a concrete `baseUrl`. Preserve hand-written Bedrock credential/profile-region resolution and Vertex's explicit-token-or-gcloud path; keep request signing in code. Neither is SDK discovery. |
| `pa-daemon/src/worker.rs` reads `PRIME_AGENT_MODEL_PROVIDER` and `PRIME_AGENT_MODEL`; `pa-core/src/session_engine/rlm_host.rs` reads `RLM_MAX_DEPTH`; `session_engine/request_timing.rs` reads its timing flag | Use existing settings/explicit invocation inputs, not independent ambient overrides. Obsolete non-secret env values are refused and reported by name with the replacement setting. |
| `pa-cli/src/config.rs`; `pa-types/src/platform/dirs.rs`; worker launch code | Keep agent/session/package/socket bootstrap paths and worker protocol environment in code. They must work before configuration loads and retain explicit-flag precedence. |
| `pa-ai/src/registry.rs`; `pa-ai/src/providers/`; `pa-ai/src/oauth/`; `pa-core/src/auth/` | Keep adapter implementations, wire constants, OAuth protocol/refresh, AWS/Google ambient credential discovery, validation bounds, and secret storage. Prime API/frontend endpoints become the user-only `login` record. Fixed OAuth client registrations and associated endpoints stay with their protocol implementation; they cannot be supplied by a project. |

The strict adapter and Prime login shapes above complete the common route.
They are not hidden in `compat` or accepted as arbitrary JSON.
`PI_CACHE_RETENTION`, AWS transport diagnostic controls, OS/terminal variables,
test hooks, offline/bootstrap variables, and updater controls are not all the
same kind of input. Keep their current behavior unless this table explicitly
moves them. In particular, preserve Bedrock's credential/profile-region reads,
Vertex's explicit token or gcloud lookup, worker coordination and platform
environment support. No SDK replacement is authorized here.

## State and deletion

Configuration, discovery data, and secrets have separate roots and authority.
They are not three names for the contents of `settings.json`.

| Kind | Location and lifetime |
| --- | --- |
| Configuration | Repository `config/` plus generated `config.json`; user `A/config.json` and `A/extensions/`; project `.prime/agent/config.json` and `.prime/agent/extensions/`. Designed for non-secret data, but do not commit local trust policy or claim arbitrary plugin config/command arguments are safe: those can embed secrets outside the host's typed references. Runtime never deletes these files. |
| Generated cache/discovery | `A/cache/`, with catalog files under `A/cache/models/`. Existing `provider-model-catalog.v1.json`, `default-model.v1.json`, and scoped discovery snapshots move here. The entire `A/cache/` may be deleted while stopped at any time. It changes startup discovery availability, not credentials, approvals, explicit models, or preferences. |
| Secrets | Existing `A/auth.json`, including MCP OAuth/static-token entries, with existing private permissions/locking; external secret values may come from explicitly referenced environment variables, existing AWS credential files, or the existing Vertex token/gcloud path. No project auth file. Never commit, generate into config, print, or copy into cache. |

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
shape. Unready installed provider templates are the explicit nonfatal exception
defined above, not malformed files. Do not silently ignore a broken project
layer or select another model.

Detect old user/project `settings.json`, `models.json`, and `ollama-cloud.json`.
For projects check the Git-root config directory and the old cwd-to-root ancestor
config locations visited during discovery, so a nested invocation cannot silently
lose an old settings file after the boundary change. Do not scan above Git root.
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
2. Check structural schema and semantic rules: installed dialects, named route
   bindings/fallback order, discovery formats, model/cost data, credential
   references, extension filenames/IDs, duplicate keys and unsupported versions.
   Validate sparse layers and their effective composition. Assert the shared
   readiness result for disabled incomplete templates, completed/enabled routes,
   and missing account/gateway/location fields. Unready templates must report
   and allow startup; malformed declarations fail. Reject `catalogs` in every
   project/extension record, even when disabled.
3. Prove precedence using whole resolved documents and provenance: nested merge,
   array replacement, disabling, skill collisions, self-aliasing and isolated
   projects. Switch `auth` from env to store and assert whole-object equality:
   no env member survives. Switch MCP HTTP to stdio and assert no URL or headers
   survive. Change region/profile/project/location alone and assert inherited
   auth/secret headers, resolved routes and approval are invalidated.
4. Round-trip Cloudflare's two routes and Bedrock's four regional models to their
   exact original provider/model/API/base-URL tuples and complete metadata.
   Run `single-route-shorthand-overlay` from the representation section with
   whole-object equality, not only standalone shorthand expansion. Verify
   independent normalization of each layer and explicit fallback order, including
   unavailable routes. Never split provider identities.
5. For each source of `maxTokens` (generated distribution, embedded copy,
   bundled snapshot, cache, fresh discovery, user model definition/override and
   project model definition/override), compare the whole resolved model and
   provenance, then capture its request body against a local fake server.
   Metadata at or above the existing 32,000 cap must remain non-explicit and
   send the capped amount; an authored explicit value must send that value.
   Use fixtures above/below the cap from existing
   `providers/simple_options.rs` and
   `providers/openai_completions/stream/tests/stream_max_tokens.rs`, not newly
   chosen output defaults. Copying a shipped value through generation/cache
   must not flip its flag or wire value.
6. Add one synthetic provider using only a JSON extension file. Use a local fake
   OpenAI-compatible inference/discovery server. It must appear in selection,
   authenticate via a reference, stream a turn, and survive offline use with
   static models, with no provider-specific Rust match or compiled-table edit.
7. Prove generation cannot replace an edited overlay or existing `.next` file;
   concurrent preference writes cannot overwrite unrelated edits. Prove update
   and startup perform no configuration writes.
8. Prove unapproved config-bearing and skill-only projects cannot execute,
   contribute skills or discover anything. Exercise a nested cwd: resources up
   to Git root require canonical-path approval; resources above it never load.
   Verify content, endpoint and effective account-selector changes lose approval.
9. Use two fake HTTP origins. Give the inference route Authorization, Cookie,
   API-key and another fixture credential-bearing header. Cross-origin discovery
   without its own auth/headers must fail before sending. With explicit discovery
   credentials, the second origin receives only its declared headers; a redirect
   cannot forward any secret-bearing header to the other origin. Assert both
   captured request objects, not only status codes. Also reject literal sensitive
   MCP headers, secret env values without references and URL userinfo. Capture
   logs/config/cache and verify fixture secrets never appear.
10. Run the offline discovery fixtures below against local fake servers, not
    live providers. They assert refresh outcomes and model snapshots, not merely
    structural validation of the input config.
11. Delete only the isolated cache and show explicit models, credentials, trust,
    preferences, and sessions remain usable. Verify stale discovery publications
    are rejected after route/auth changes. Test a hanging discovery server so
    startup and first paint remain responsive.

| Proposed fixture | Required assertion |
| --- | --- |
| `unknown-openai-without-defaults` | A fake OpenAI discovery response contains a known local ID and an unknown ID, with no complete local defaults. Refresh succeeds; the unknown ID is absent; the complete known local model survives unchanged; a credential-free warning names provider, route and unknown model. |
| `unknown-openai-with-defaults` | Use the same response and unknown ID, now with complete explicit local defaults for context/output, reasoning, input and all four cost fields. Refresh succeeds and admits the unknown ID with the expected whole model and route binding; known local models survive. Defaults copied into discovered metadata do not mark `maxTokens` explicit. |
| `unknown-openrouter-incomplete-metadata` | A fake metadata-rich OpenRouter response omits one required cost rate for an unknown ID and has no complete local defaults. Refresh succeeds; the unknown ID is absent; the known local model survives unchanged; a credential-free provider/route/model warning explains incomplete metadata. Do not synthesize a zero rate. |

These fixture names describe tests to add during implementation, not tests that
already exist. Capture complete refresh reports, model objects and warnings.
Include fixture secrets in fake request headers and assert none appear in warnings.

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
| What is the project boundary? | Use the containing Git worktree root, triggered by session cwd, and stop ancestor resource discovery there. Outside Git use cwd with no ancestor search. | This intentionally changes today's settings boundary. A broader scan could import unrelated executable resources; a narrower one could bypass root config or skill-only approval. |
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
