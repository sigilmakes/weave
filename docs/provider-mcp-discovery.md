# Provider and MCP discovery as data

Status: proposed design for #67; design only. No runtime, schema, CI or test-harness
change is made here. Week 2.5 of the suite remains open.

This extends [Configuration as data](configuration-as-data.md), merged in
`57cd83b17`. Source citations below refer to that commit unless otherwise marked.
The #65 implementation is concurrent, not evidence that these proposed fields
already exist. Implement #65 first; keep its schema version 1, strict parsing,
layer normalization, trust gate, route replacement and credential references.
The additions below must be introduced together in that schema and its reader.
Do not add a parallel configuration loader or provider registry.

## 1. Decisions and source evidence

A provider using an installed inference protocol and an installed discovery
format is configurable with data alone. Request shaping means selecting a
protocol, destination, supported options and discovery request parameters; it
does not mean implementing a streaming protocol with templates. Response mapping
means selecting a named, versioned parser contract. Arbitrary JSONPath, scripts,
expressions, body templates, remote endpoints and remote auth instructions are
not supported in v1.

Evidence for the existing seams (quotes are from actual source):

| Source | Evidence and consequence |
| --- | --- |
| `docs/configuration-as-data.md:187-262` | “A provider ID is not a transport API ID.” Keep named routes and derive a model's API/base URL from its binding. |
| `docs/configuration-as-data.md:321-395` | “No environment-variable name is inferred from an ID.” Keep explicit references and complete unknown-model metadata. |
| `crates/pa-core/src/models/registry/catalog_sources.rs:16-26,35-44` | `builtin_format` and `protocol_route` currently match provider/protocol strings. Replace provider membership with #65 data, not another match per provider. |
| `crates/pa-models/src/discovery.rs:29-68` | `discovery_url` builds `/models`, Anthropic `/v1/models`, Codex `/codex/models`; it appends `client_version=0.159.0` or `pageSize=1000`. Extract these choices to reviewed request data. |
| `crates/pa-models/src/discovery.rs:85-95,131-158` | `ResolvedSource` holds URL/headers; `Metadata` has no transport URL/auth fields. Preserve that boundary. |
| `crates/pa-models/src/fetch.rs:85-90,146-158` | `.redirect(reqwest::redirect::Policy::none())` and “Catalog request refused a redirect”. Preserve refusal rather than trusting HTTP-client header stripping. |
| `crates/pa-models/src/discovery.rs:175-194` | `identity` covers source/provider/format/URL; `scope` also covers normalized request headers. Extend the scope to the resolved #65 route and approval generation. |
| `crates/pa-models/src/discovery.rs:868-894` | Current projection assigns remote name, limits, capabilities and costs. #67 makes the field ownership order below explicit instead of letting this assignment overwrite user choices. |

### What remains code

Installed adapters own inference JSON, streaming/SSE framing, protocol suffixes,
error decoding, signing and protocol validation. The route's `dialect`, `compat`,
`adapterOptions`, `inferenceParams` and `modelParams` remain #65 controls;
section 2 adds typed endpoint/request choices. Unknown options fail; no raw
request-body overlay is added.
An auth scheme cannot turn an OpenAI request into an Anthropic request.

The entire xAI subscription profile stays code, not partly generated data:
`crates/pa-core/src/models/registry.rs:106-121` sets `openai-responses`,
`https://api.x.ai/v1`, and compatibility conditionally; lines 127-162 implement
named thinking maps and the all-null unknown-model fallback. Preserve the
conditional application at lines 578-584. No new subscription schema is decided.

Bedrock remains the hand-written owner in
`crates/pa-ai/src/providers/bedrock/auth.rs:65-92,224-319`, including credentials,
profile/region precedence, fixed-gateway handling and SigV4. Vertex remains the
hand-written owner in `crates/pa-ai/src/providers/google_vertex.rs:59-120`:
`GOOGLE_OAUTH_ACCESS_TOKEN` then `gcloud auth print-access-token`, not an SDK ADC
chain. Neither gets a generic models-list request invented by this design.
A route without a discovery declaration uses authored/offline models only.
The environment-detection sentinel in `crates/pa-ai/src/env_api_keys.rs:62-105` is not proof that
these owners support container/web-identity SDK credentials.

## 2. Exact data contract

All objects use #65 strict parsing: duplicate/unknown keys and unknown tags fail.
URLs must be HTTP(S), without userinfo, query or fragment. HTTPS is required
except literal loopback HTTP development/fake servers; no credentialed remote
HTTP. Same origin means scheme, canonical host and effective port, not suffix
matching. A route change replaces the complete route and references as #65
requires. All new members below belong to that atomic route identity.

Keep #65 `discovery.format`, `url`, `headers`, `auth`, `defaults`. Add:

| Member | Contract |
| --- | --- |
| Route `authScheme` | Optional tagged scheme below; omitted means installed dialect's scheme. |
| Discovery `authScheme` | Optional tagged scheme; omitted means selected format's scheme, not the inference dialect's. |
| Discovery `request` | Optional `{method: "GET", query: {<name>: <literal string>}}`. Both members required when present. No body, environment interpolation, secret references or arbitrary method. Omission means GET with the selected format's existing query defaults. Presence replaces the complete query defaults, not merges them. |

`auth` selects a credential; `authScheme` specifies how its value is sent.
Allowed schemes are `{kind: "none"}`, `{kind: "bearer"}`,
`{kind: "header", name: <HTTP header name>}`, and
`{kind: "native"}`. Header inserts the raw referenced credential; bearer inserts
`Authorization: Bearer <value>`. No custom prefix or query/body credential.
Native delegates only to the selected installed owner (OAuth/account headers,
Bedrock signing or Vertex token acquisition). Invalid combinations fail:
`auth.none` requires scheme none; ambient requires matching native owner; an
API-key env/store reference requires bearer/header unless the installed native
owner explicitly accepts it. No default credential is inferred from a scheme.
An absent optional scheme preserves existing format/dialect behavior:
Anthropic uses `x-api-key`, Google `x-goog-api-key`, OpenAI/OpenRouter/Ollama
bearer, and Codex/Prime use their native owner for associated account headers.
The same scheme override is allowed for ordinary bearer/header inference
adapters only; reject overrides of Bedrock, Vertex and subscription native auth.

Header names compare case-insensitively. Reject duplicate normalized names,
CR/LF, Host, Content-Length and hop-by-hop headers. Reject any configured header
that conflicts with generated auth, rather than letting iteration order choose.
Authorization, Proxy-Authorization, Cookie, API-key headers and every
credential-reference-valued header are sensitive. Literal secrets are forbidden;
non-secret header values are literal strings, never implicit env lookups.
Treat unknown custom headers as potentially sensitive for diagnostics. No cookie
jar, netrc, proxy auth or default client header can add credentials to a request.
Explicit process proxy transport configuration must not forward origin secrets
as proxy credentials.

`discovery.url` is the final listing endpoint, not an inference base. A complete
provider record in distribution data supplies it explicitly. For compatibility
with #65's optional URL, omission uses the existing installed format derivation
only; do not probe guessed endpoints after a failure. Distributions should emit
explicit URLs/query data when extracting existing constants. `request.query`
is URL-encoded by the HTTP library; reserve provider pagination keys to the
parser owner and reject collisions. No raw query string in the URL.

Example: a new OpenAI-compatible provider, without any new Rust provider ID:

```json
{
    "schemaVersion": 1,
    "providers": {
        "example": {
            "enabled": true,
            "route": {
                "dialect": "openai-completions",
                "baseUrl": "https://inference.example/v1",
                "auth": {"env": "EXAMPLE_INFERENCE_TOKEN"},
                "authScheme": {"kind": "bearer"},
                "discovery": {
                    "format": "openai",
                    "url": "https://catalog.example/models",
                    "request": {"method": "GET", "query": {}},
                    "auth": {"env": "EXAMPLE_CATALOG_TOKEN"},
                    "authScheme": {"kind": "header", "name": "x-catalog-key"},
                    "headers": {},
                    "defaults": {
                        "name": "Example model",
                        "reasoning": false,
                        "input": ["text"],
                        "cost": {"input": 1, "output": 2, "cacheRead": 0, "cacheWrite": 0},
                        "contextWindow": 64000,
                        "maxTokens": 4000
                    }
                }
            }
        }
    }
}
```

Example numbers are authored choices for this fictional provider, not new
runtime defaults. Credential names are illustrative, not added built-in aliases.

### Inference endpoint and request fields

Add route `endpoints` and `request`; these are separate from
`discovery.request`. They replace #65's fixed protocol-suffix limitation, not
its protocol ownership. `endpoints` is a strict object with the one operation
key accepted by its dialect: `chat` for openai-completions, `responses` for
openai-responses/openai-codex-responses/azure-openai-responses, `messages` for
anthropic-messages. Each value is an absolute path suffix beginning with one
slash; reject scheme/authority, query, fragment, backslash, dot segments and
encoded slash/backslash/dot traversal. Join base path, pathPrefix and suffix
at one slash without resetting the base path. For example base
`https://gw.example/proxy`, prefix `/tenant`, chat `/custom/chat` yields
`https://gw.example/proxy/tenant/custom/chat`. Never append a second default
suffix. Omission uses the dialect's current suffix. Bedrock/Vertex/Google and
other native operations reject arbitrary suffix overrides in v1 because their
resource paths and signing/URL parameters remain code-owned.

Current suffix evidence: `crates/pa-ai/src/providers/anthropic/stream.rs:245`
uses `"{}/v1/messages"`; `crates/pa-ai/src/providers/openai_completions/stream.rs:632`
uses `"{}/chat/completions"`. Extraction must include Responses/Codex/Azure's
existing final paths and preserve the same whole URL, not add another `/v1`.

A route `request` is a strict dialect-specific object. Members are optional;
omission uses the reviewed distribution value for that route, then the installed
dialect default. A replacing complete route carries its own request policy,
never a previous destination's. Distribution extraction must supply every
provider-sensitive choice; the runtime must not infer one from a provider name
or URL substring. The fields below select installed behavior, not raw JSON.

| Field | Values and exact effect |
| --- | --- |
| `store` | `"omit"`, false, true. OpenAI chat/Responses only: omit the wire member or send that boolean. Default for unknown compatible route is omit; explicit true is a disclosed persistence choice requiring project approval. |
| `stream` | Exactly true for the three streaming dialects. False is rejected as unsupported, not sent to an SSE parser. Other native adapters keep installed behavior and reject this field. |
| `maxTokensField` | Chat only: `"max_tokens"` or `"max_completion_tokens"`; write exactly that key. Responses uses installed max_output_tokens, Anthropic installed max_tokens and rejects this override. |
| `reasoning` | Chat: `"none"` or existing ThinkingFormat strings openai/openrouter/deepseek/zai/qwen/qwen-chat-template. None emits no reasoning controls. Other values select existing serializers and model thinking maps; they do not set a reasoning level. Responses/Anthropic allow only `"native"` or `"none"`; native preserves their installed budget handling. |
| `cacheControl` | Chat: `"none"`, `"openai"`, `"anthropic"`. None emits neither prompt cache keys nor cache-control blocks; openai uses session prompt_cache_key and existing cache-retention rules; anthropic selects existing block decoration. Responses/Anthropic allow native/none only. No raw cache header/body template. |
| `toolCallIds` | Chat: `"openai"` or `"compatible"`, selecting current ID normalization with or without the OpenAI non-pipe 40-character truncation. Both keep existing pipe-ID sanitation. Responses/Anthropic accept native only. IDs are remapped consistently for calls/results; canonical session history is unchanged. |
| `serviceTier` | Chat: `"send"` or `"omit"`, replacing provider-name gating for an explicit invocation tier. |

For chat, request.maxTokensField/reasoning/cacheControl and existing compat
maxTokensField/thinkingFormat/cacheControlFormat overlap. Do not keep two
sources: normalize each layer's legacy-compatible compat member to the matching
request member, remove it from normalized compat, and reject both spellings in
one layer even when equal. supportsStore normalizes to store=false/omit;
supportsReasoningEffort remains a capability, not the selected format.
Responses/Anthropic native choices do not add new budget algorithms. Reject a
field unsupported by the selected dialect rather than silently ignoring it.

Current request anchors: `crates/pa-ai/src/providers/openai_completions/params.rs:28-60`
sets stream, URL-selected prompt cache, store and token key; lines 93-189 own
reasoning formats and 196-210 contain provider/URL routing checks.
`crates/pa-types/src/ai/compat.rs:8-45,48-71` defines the existing compat enums;
`crates/pa-ai/src/providers/openai_completions/convert.rs:362-382` contains
pipe sanitation and OpenAI truncation. Existing OpenRouterRouting and
VercelGatewayRouting options remain typed compat data; a present routing object
selects its installed serializer, not a URL match. Any remaining provider/URL
branch must be inventoried during implementation as either a shipped data
choice or one of the explicit native exceptions. Do not claim the issue's
branch-removal acceptance merely from this prose.

### Response mapping selected by `format`

No new arbitrary mapping language is needed for the installed formats. Their
mapping is fixed code selected by data; changing a mapping is adapter work.

| Format | Mapping contract |
| --- | --- |
| `openai` | `data[]`, `id`; metadata-poor. |
| `anthropic` | `data[]`, `id`, `display_name` to name; metadata-poor. |
| `google` | `models[]`, `name` minus leading `models/`, `displayName`, `inputTokenLimit`, `outputTokenLimit`; reject explicit lack of `generateContent`. |
| `codex` | `models[]`, `slug`, `display_name`, `context_window`, `input_modalities`, supported reasoning levels; non-list visibility denies admission. |
| `openrouter` | `data[]`, `id`, `name`, `context_length`, `top_provider.max_completion_tokens`, architecture modalities, reasoning parameters, pricing prompt/completion/input_cache_read/input_cache_write. Per-token prices become per-million prices. |
| `ollama` | Installed native listing and detail owner; normalize to listed IDs, capabilities and model_info, not an OpenAI list. |
| `prime-inference` | Installed public/private owner; entitlement filtering and native parameter/effort mapping stay code. Never interpret catalog fields as route/auth authority. |

Exact existing mapping anchors: `crates/pa-models/src/discovery.rs:599-646,649-749,752-815`;
Prime normalization is at lines 496-595. Native Ollama ownership begins at
`crates/pa-models/src/ollama_cloud.rs:1-18,41-65`. Keep its pagination/detail and
size bounds in code. Remote next-page URLs are not followed: accept only native
opaque pagination tokens and construct the next request on the original origin
and endpoint. Any embedded URL/account/header/config field is ignored, not cached.
The same origin rules apply to every detail/page request. Publish only after
all required pages validate; an incomplete refresh is not an empty result.

### LiteLLM and Bifrost formats

Add installed format tags `litellm` and `bifrost`; this is parser implementation
work, not a claim those formats exist today. Both require explicit discovery.url
and auth/headers. Never probe management endpoints with inherited credentials
from a different origin. Their remote upstream provider labels do not create
new Weave provider identities or choose inference destinations.

| Format | Accepted envelope and mapping |
| --- | --- |
| `litellm` | GET authored `/model/info` (or deployment-specific full URL); root `data` array. Each row has nonempty `model_name` (public inference ID, also display-name fallback) and object `model_info`. Map model_info.max_input_tokens to contextWindow, max_output_tokens to maxTokens, supports_vision boolean to text/text+image, supports_reasoning boolean to reasoning. Map input_cost_per_token/output_cost_per_token/cache_read_input_token_cost/cache_creation_input_token_cost to the four costs, multiplying each by 1,000,000. Do not interpret legacy max_tokens as both input and output. |
| `bifrost` | GET authored `/api/models/details`; root `models` array and nonnegative integer `total`. Row has nonempty `provider` and `name`; inference ID is provider/name, unless name already starts with exactly provider/. Map context_length to contextWindow, falling back only to max_input_tokens; max_output_tokens to maxTokens; architecture.input_modalities to text/image input. Map the same four per-token cost keys as LiteLLM. Reasoning is not specified by this endpoint's published shape: require authored reasoning defaults. No model-ID heuristics. |

LiteLLM only accepts absent mode or mode=chat; non-chat rows are excluded with
unsupported_mode warning. A missing model_info member does not trigger another
endpoint: that unknown ID needs complete explicit defaults or is dropped. Ignore
litellm_params entirely, especially api_key/api_base/project/credential fields;
do not retain the raw row. Duplicate model_name rows are rejected unless their
normalized allowed metadata is identical, in which case collapse them; divergent
deployment metadata produces ambiguous_model warning and excludes the ID.
This is the one format-specific exception to the duplicate-source error rule.

Bifrost accepts only text-producing input/output modality metadata. Missing
modality, reasoning or prices must come from explicit defaults; no absent value
implies false/zero. Ignore accessible_by_keys and additional_attributes; do not
cache key IDs. Never set unfiltered=true or enumerate provider keys. Keep default
filtered access semantics. Reserve offset/limit query keys to the parser; use
pages of 20 (the published endpoint default), start at offset zero, and advance
by returned row count until total is reached. Empty page before total, changing
total or duplicate IDs fails the whole refresh; do not publish partial results.
The existing request time/byte bounds also bound the complete pagination job,
not a fresh unlimited budget per page.

Sources consulted 2026-11-21: [LiteLLM pricing Sample Spec](https://docs.litellm.ai/docs/provider_registration/add_model_pricing)
explicitly distinguishes max_input_tokens/max_output_tokens from legacy
max_tokens and documents per-token prices/capability booleans;
[Bifrost List model details OpenAPI](https://docs.getbifrost.ai/api-reference/providers/list-model-details)
defines GET /api/models/details, models/total, field names, offset/limit and
unfiltered semantics. These are external contracts, not repository code
citations. The LiteLLM /model/info envelope is the proposed supported v1 shape;
implementation must capture a sanitized real response with a deployment/version
label before claiming compatibility, not infer it solely from pricing docs.
Bifrost /v1/models is deliberately not treated as this metadata-rich format;
configure it as openai plus complete defaults instead.

Required parser fixtures compare the entire normalized model, not just IDs:
complete LiteLLM metadata, duplicated deployment aliases, missing cache price,
Bifrost paginated details and missing reasoning defaults. Use saved sanitized
responses plus local fake servers, never production credentials. If a deployed
version has a different envelope, report unsupported_shape and add a reviewed
parser version; do not silently guess alternate keys.

## 3. Model admission and one source of truth

Identity is `(provider ID, route name, model ID)` internally. The existing public
provider/model selector must be unambiguous: if two routes discover the same ID,
an authored model binding chooses one; otherwise exclude that discovered ID and
warn `ambiguous_route`. Never choose the fastest response or first map entry.
A remote row cannot rebind an authored model to another route.

A new discovered model must have a valid nonempty ID (no control characters,
existing 1,024 UTF-16-unit maximum; retain stricter native-format ID bounds), a
nonempty name, boolean reasoning, nonempty text/image input containing text,
positive integral contextWindow/maxTokens representable by the model types,
and four finite nonnegative per-million cost values. Optional thinking maps use
existing level names/null semantics. Name may fall back to the validated ID;
this is a display rule, not an invented capability. Unknown cost is not zero.
No new 32,768/8,192 or 128,000/16,384 defaults are allowed.

OpenAI/Anthropic/Google/Codex unknown IDs require complete authored discovery
defaults even if a response supplies some fields. Metadata-rich formats may
supply the complete shape, with authored defaults filling missing fields. A
present invalid field does not become valid by substituting a default: drop
that unknown row. A known local model keeps its complete local definition when
an incomplete/invalid update cannot produce a valid projected object.

Projection order is **per field**, not another configuration precedence layer:

| Fields | Source and precedence, highest first |
| --- | --- |
| Provider, route, dialect/API, endpoint, auth, headers, compat, adapter options | Effective authored route only; remote/cache never participates. |
| Model ID | Valid remote ID for new rows; authored key for existing entries. It cannot be changed by metadata. |
| Name, reasoning, input, costs, contextWindow, maxTokens, thinking map | Explicit user/project model definition or modelOverride field following #65 scope order; then valid fresh or same-scope last-good discovery field; then authored model field from distribution/curated baseline; then explicit route discovery defaults for missing fields. |
| New-ID metadata-poor shape | Complete explicit discovery defaults required before the preceding field projection; no borrowing the first shipped model as an implicit template. |
| max_tokens_explicit | True only for an authored user/project model/override maxTokens write, including that scope's extension; false for discovery defaults, distribution, bundled and cache metadata. Never deserialize this flag from remote data. |
| Limits | Apply authored route limits after metadata projection; preserve #65 explicit request-token handling and source provenance. |
| Access/availability | Current local approval, credentials and native entitlement owner. A denial wins over every cached metadata value. |

Within the explicit override tier #65's distribution/user/project precedence
remains unchanged. Trusted seeds overlay the baseline; the table then governs
discovery projection and reapplies explicit field choices. Baseline fallback is
validated curated cache, bundled snapshot, shipped offline data, as #65 states.
Keep each field's source and whether it was explicitly authored. Do not write
the effective object back into any config file. Cache stores normalized remote
metadata and provenance, never a fully resolved route/model that can become an
alternative authored authority. Recompute from current config on every load.
The existing 32,000 request cap remains unchanged
(`crates/pa-ai/src/providers/simple_options.rs:7-29`).

Drop unknown incomplete/invalid rows with a warning containing safe provider,
route, model ID, reason code and missing/invalid field names. Aggregate repeated
warnings per refresh and include counts. Escape control characters and limit
remote display text before showing it. Never quote raw row JSON or server text.
Malformed container JSON or a nonempty list with no valid IDs fails the source;
individual rejected rows otherwise make a successful refresh with warnings.
Reject duplicate IDs within one source as a source error, not last-row-wins.
An absent ID in a successful full refresh removes only its discovery-only
projection; authored/offline definitions remain. Explicit native entitlement
revocation removes private availability even when metadata still exists.

## 4. Credentials, origins, redirects and privacy

For same-origin discovery, omitted auth/headers inherit route auth/headers.
Explicit auth replaces credential selection; explicit headers replace the
header map. Cross-origin discovery requires both explicit auth (possibly none)
and explicit headers (possibly empty), and inherits **nothing**, including
non-secret route headers. Missing declarations fail before the first send.
Headers and credentials are resolved after origin checks, never before a
cross-origin inheritance decision. No provider-ID ambient lookup bypasses this.

Refuse all automatic redirects for provider catalog, native detail, MCP catalog,
MCP HTTP and OAuth metadata requests, even same-origin. A moved endpoint requires
authored data and, for project data, new approval. An OAuth protocol browser
redirect explicitly handled by its installed auth owner is not an HTTP-client
redirect exception for discovery. New OAuth registrations are out of scope.

Worked redirect example (values are placeholders, never real credentials):

```text
GET https://api.example/v1/models
Authorization: Bearer <route-secret>
x-api-key: <key-secret>
Cookie: <cookie-secret>
x-private-token: <custom-secret>
chatgpt-account-id: <account>
x-client-label: desktop
Accept: application/json

302 Location: https://other.example/models
```

Outcome: `redirect_refused`, no second request. The refused destination receives
none of Authorization, x-api-key, Cookie, x-private-token, chatgpt-account-id,
x-client-label or Accept: all headers are dropped with the request, not merely
three conventional sensitive headers. We do not strip auth and silently retry.
If a person authors `https://other.example/models` as a new cross-origin discovery
endpoint with `auth: {none: true}` and `headers: {}`, a new independent GET carries
only generated protocol headers such as Accept and cache-control. No account or
route header survives. The explicit cross-origin credential example in section 2
instead sends only x-catalog-key and its generated protocol headers.

Account/project/location/region/profile/gateway identifiers may appear in
explicit typed adapter options, an authored URL path or supported region host
binding, the owning native protocol's account header, and the local approval
view. They are not credentials and must not be token interpolation sources.
Codex's token-derived account header is scoped to its authorized origin
(`crates/pa-core/src/models/registry/catalog_sources.rs:266-276,546-551`); do not expose decoded claims or reuse it
for an unrelated listing URL. Prime team headers follow the same rule.
The approval view shows the complete non-secret destination and selectors,
credential reference names and their sources, never resolved values. If the
effective account selector changes, invalidate approval and cached publication.

Routine logs/report/cache paths use source IDs, status/reason codes, counts,
field names, timestamps and opaque scope IDs, not raw URLs with account paths.
Log a sanitized origin only when needed; never query values, account identifiers,
headers, cookies, tokens, OAuth codes/verifiers, decoded claims, request/response
bodies, auth file contents or upstream error text. Credential references may be
shown in explicit local config/approval diagnostics, not resolved values.
Sanitize transport errors rather than printing `reqwest` errors containing URLs
(the current `crates/pa-models/src/fetch.rs:140-145,174-179` needs this change). Discovery caches must
not contain account selectors or header values; use the existing scoped cache
machinery, never serialize the resolved request. Private metadata remains local
and must not be included in telemetry or automatic uploads.

## 5. MCP listing, cache and failure representation

MCP discovery means capabilities of configured/connected servers, not a remote
service directory. `.changes/remove-mcp-service-catalog.md:1` says “The MCP service
catalog is gone”; `prime-agent-runtime/src/rlm/mcp.py:3-12` describes live named
connections. Do not restore removed list_plugins/search_plugins/list_connections
or mcpCatalogSources surfaces. Neither MCP discovery nor configuration activates
a plugin or registers an inference provider. No remote row starts a process or
creates a connection automatically.

`MCP_SERVICE_CATALOG_URL` still exists in `crates/pa-models/src/fetch.rs:12-14`,
but it is not authority to restore the retired service directory. Do not add a
catalogs.mcpServices setting, fetch that URL, or create a service-directory cache.
A future service directory needs a separate product decision and migration.

Current MCP evidence: `crates/pa-core/src/mcp/mod.rs:649-749` builds the
configured roster and defines connected as credentials-present plus enabled,
not network readiness. `crates/pa-daemon/src/mcp_connections.rs:21-46` serializes
that roster. `prime-agent-runtime/src/rlm/mcp.py:313-373` publishes tools only
after complete pagination, lines 453-556 own per-server locks/epochs/cache and
lines 623-652 return per-server tools/error. Keep the new discovery state
separate from the current config/auth connected flag. Existing initialized
connections may continue while a list_changed refresh is pending; removed
handles become invalid at atomic publication, not during partial page receipt.

The runtime HTTP code at `prime-agent-runtime/src/rlm/mcp.py:264-289` explicitly
sets follow_redirects=False on its custom-client path but delegates SDK headers
on another path. Implementation must enforce refusal on **both** paths; the
current partial coverage is not security evidence. Native OAuth HTTP already
refuses redirects (`crates/pa-core/src/mcp/oauth_http.rs:82-110`); its issuer/
resource refresh binding is checked in `crates/pa-core/src/mcp/oauth.rs:412-534`.

Servers are declared in #65 `settings.mcpServers`, keyed by stable configured
name. HTTP and stdio remain tagged records and replace atomically. List every
configured server, sorted by name, including disabled, unapproved and failed
ones; include transport kind, safe origin or executable label, enabled/approval
state and latest discovery state. Do not reveal URL path/query or command args
in routine listing. Built-in integrations remain explicitly owned local entries, not remote directory rows.

A connection's initial state is pending, then ready only after MCP initialize,
capability negotiation and complete tools/list pagination succeed. Status values
are disabled, unapproved, pending, ready, failed. A failed record retains its
configured name, safe endpoint label, stable reason code, last attempt time and
optional last-success time; tools are empty. Distinguish auth_required,
auth_rejected, not_found, timeout, transport, protocol and invalid_metadata.
One failed server never hides the rest or aborts first paint. A ready server
with an empty successful tools list has toolCount zero, not failed.

Connected-server capabilities/tools are memory-only in v1, scoped to server
config digest, current connection and credential/approval generation. There is
no new MCP disk cache. Existing durable OAuth grants stay in the secret store,
never a tool cache. Retained last-success
information is diagnostic only: failed/disconnected/retired servers expose no
callable stale tools. Reconnect reinitializes and relists; tools/list_changed
refreshes this server only, replaces its complete inventory atomically and
invalidates removed callable handles. Do not execute cached schemas as code.

For MCP HTTP use the same no-redirect and origin/header rules as section 4.
OAuth protected-resource/authorization metadata from another origin is public
metadata only: send no server headers, tokens, account headers or cookies.
The installed MCP auth owner validates issuer/resource binding and owns token
requests. A metadata URL is not consent to send resource credentials there.
Stdio launches only an approved configured executable with explicitly selected
non-secret environment and referenced secrets; do not dump process environment
or commands into errors. No broad provider-header inheritance into MCP exists.

## 6. Refresh, readiness and failures

Installed/complete/authenticated/available are independent. #65's disabled or
incomplete templates remain unready and make no requests. Missing secret means
unauthenticated, not malformed configuration. A 404 does not make a syntactically
complete inference route malformed. Refresh runs off first paint; explicit
refresh awaits a job without blocking the interactive frame path.

| Event | Outcome and publication |
| --- | --- |
| Discovery returns 404 | Failed/not_found for this source; no guessed /v1 alternative, no cross-route retry. Retain same-scope public last-good metadata marked stale and authored/offline entries. Inference readiness is unchanged. No cache means authored-only availability. |
| Valid regional list is empty | Successful updated count zero, not unready. Replace that region's discovery snapshot with empty. Do not borrow models or credentials from another region; authored models retain their binding. Exact selection that has no usable model reports unavailable. |
| Timeout, 5xx, malformed JSON, incomplete pagination | Failed with stable reason; do not publish a partial inventory. Retain same-scope last-good public data as stale; other sources proceed. |
| 401/403 or token expires during any page/detail request | Auth rejected; discard the entire attempted publication, revoke that credential scope and private grants. Never treat it as successful empty. OAuth refresh is only through existing auth owner; no generic discovery retry loop. After owner refresh, start a new whole refresh with a new credential generation. Static env tokens require replacement. |
| Token/route/project approval changes during refresh | Cancel/discard the old generation before memory and disk publication, even if HTTP succeeded. New generation cannot adopt old-scope private data. |
| Missing model fields | Per-row warning/drop or keep known local model as section 3; not whole-startup failure. |
| MCP server fails | Failed record remains listed, zero callable tools; other servers/providers remain ready. |
| Cache write fails | Report cache_write; accepted current in-memory metadata may serve this process, never claim durable success. |

A 401/403 must also hide scoped metadata from concurrent/older processes using
the existing revocation mechanism (`crates/pa-models/src/cache.rs:384-440`).
Offline fallback cannot restore a revoked private model. Public authored models
remain definitions, not a claim of current entitlement. Discovery failure never
switches a session's selected model or replays inference against another route.
Keep current timeout/size bounds (`crates/pa-models/src/fetch.rs:16-18`; private discovery
`crates/pa-models/src/discovery.rs:218-221`) and refresh interval, not newly tuned constants.

## 7. Explicit migration inventory

#65 owns the configuration cutover. This design completes its endpoint/auth
inventory; do not implement an env fallback alongside the new data. Detect
obsolete non-secret overrides before activation, report **names only** and the
replacement pointer, refuse ambiguity even if config is also present. Never
print their old values. Initialization/validation stay offline-capable.

| Existing input, with source | Replacement/action |
| --- | --- |
| OLLAMA_API_BASE (`crates/pa-models/src/chain.rs:35-39`) | Complete ollama-cloud route baseUrl and explicit discovery URL. Refuse old override. |
| PRIME_AGENT_INFERENCE_API_BASE_URL, PRIME_AGENT_INFERENCE_FRONTEND_URL (`crates/pa-core/src/auth/prime_inference.rs:39-51`) | User-only providers.prime-inference.login.apiBaseUrl/frontendUrl. Refuse old overrides. Inference baseUrl is a separate route. |
| AZURE_OPENAI_BASE_URL, AZURE_OPENAI_RESOURCE_NAME (`crates/pa-ai/src/providers/azure_openai_responses.rs:134-150`) | Concrete route.baseUrl; translate resource-name form using its existing URL construction at lines 121-123. Refuse old overrides. |
| AZURE_OPENAI_API_VERSION, AZURE_OPENAI_DEPLOYMENT_NAME_MAP (`crates/pa-ai/src/providers/azure_openai_responses.rs:61-68,126-132`) | route.adapterOptions.azureApiVersion/deploymentNames, preserving map parsing result and v1 default. Refuse old overrides. |
| GOOGLE_CLOUD_PROJECT, GCLOUD_PROJECT, GOOGLE_CLOUD_LOCATION (`crates/pa-ai/src/providers/google_vertex.rs:72-93`) | route.adapterOptions.project/location and explicit regional binding; refuse these non-secret endpoint selectors after cutover. Token/gcloud owner remains code. |
| PRIME_AGENT_MODEL_PROVIDER, PRIME_AGENT_MODEL (`crates/pa-daemon/src/worker.rs:423-424`) | Existing #65 settings.defaultProvider/defaultModel or explicit invocation selection; no independent endpoint/auth layer. |
| RLM_MAX_DEPTH (`crates/pa-core/src/session_engine/rlm_host.rs:361-368`), PI_REQUEST_TIMING (`crates/pa-core/src/session_engine/request_timing.rs:29-30`) | #65 settings-owned migration, not discovery inputs. No new interpretation here. |

Credential variables remain usable only through authored references, with aliases
and ordering copied exactly from `crates/pa-ai/src/env_api_keys.rs:6-43`:
COPILOT_GITHUB_TOKEN, GH_TOKEN, GITHUB_TOKEN; ANTHROPIC_OAUTH_TOKEN then
ANTHROPIC_API_KEY; OPENAI_API_KEY, AZURE_OPENAI_API_KEY, PRIME_API_KEY,
OLLAMA_API_KEY, DEEPSEEK_API_KEY, GEMINI_API_KEY, GOOGLE_CLOUD_API_KEY,
GROQ_API_KEY, CEREBRAS_API_KEY, XAI_API_KEY, OPENROUTER_API_KEY,
AI_GATEWAY_API_KEY, ZAI_API_KEY, MISTRAL_API_KEY, MINIMAX_API_KEY,
MINIMAX_CN_API_KEY, MOONSHOT_API_KEY, HF_TOKEN, FIREWORKS_API_KEY,
OPENCODE_API_KEY, KIMI_API_KEY, CLOUDFLARE_API_KEY, XIAOMI_API_KEY,
XIAOMI_TOKEN_PLAN_CN_API_KEY, XIAOMI_TOKEN_PLAN_AMS_API_KEY,
XIAOMI_TOKEN_PLAN_SGP_API_KEY. No renaming or secret-value migration is needed.
Preserve #65 stored-before-env ordering except Prime env-before-store.

Keep GOOGLE_OAUTH_ACCESS_TOKEN and the gcloud path in Vertex's native owner.
GOOGLE_APPLICATION_CREDENTIALS currently participates in readiness detection
(`crates/pa-ai/src/env_api_keys.rs:103-117`), not token-file parsing by that owner; do not promise
new ADC behavior. Keep AWS_BEDROCK_SKIP_AUTH, AWS_ACCESS_KEY_ID,
AWS_SECRET_ACCESS_KEY, AWS_SESSION_TOKEN, AWS_PROFILE, AWS_REGION,
AWS_DEFAULT_REGION in their existing Bedrock owner. AWS_BEARER_TOKEN_BEDROCK,
AWS_CONTAINER_CREDENTIALS_RELATIVE_URI, AWS_CONTAINER_CREDENTIALS_FULL_URI and
AWS_WEB_IDENTITY_TOKEN_FILE are currently detected by `crates/pa-ai/src/env_api_keys.rs:87-95`;
this design does not make them implemented credential chains. Distinguish a
sentinel/readiness hint from successfully resolved credentials.

PRIME_AGENT_CODING_AGENT_DIR, PI_PACKAGE_DIR and other worker/bootstrap directory
variables remain bootstrap inputs per #65, not provider-endpoint overrides.
MCP's explicitly configured stdio env references remain local to that server;
there is no new global MCP endpoint environment variable.

Hardcoded endpoints move without changing their values:

| Current source | Data owner |
| --- | --- |
| `crates/pa-ai/src/models_generated.rs` model base URLs | #65 reviewed provider routes, preserving every provider/model/API/base tuple. |
| `crates/pa-models/src/fetch.rs:9-14` model and retired MCP catalog URLs | Model URL becomes catalogs.models.url. MCP constant is unused legacy data, not a new setting or fetch target; do not restore the removed directory. |
| `crates/pa-models/src/provider_catalogs.rs:5-6` default-model URL | catalogs.defaultModel.url. |
| `crates/pa-models/src/ollama_cloud.rs:12-13` BASE_URL | ollama-cloud authored route/discovery base. Native /api/tags and /api/show mechanics remain code. |
| `crates/pa-models/src/prime_inference.rs:15` inference base | prime-inference authored route. |
| `crates/pa-core/src/auth/prime_inference.rs:12-18` API/frontend/inference defaults | login profile plus route; retain immutable production triple guard for CLI credential reuse. |
| `crates/pa-models/src/discovery.rs:33-68` derived paths/query defaults | Reviewed explicit discovery URL/request data; optional derivation stays installed code as specified above. |
| xAI subscription, Bedrock signing and Vertex protocol endpoint construction | Remain the native code exceptions in section 1; do not move half a conditional behavior. |
| Fixed OAuth authorization/token/client-registration endpoints | Remain installed auth-owner code. Project data cannot redirect grants. |

Users copy deliberate endpoint/account choices into complete authored routes,
keep credentials in env/auth store, validate offline, approve the resolved local
snapshot, and only then refresh. Preserve old settings/models/ollama files for
manual rollback but move them outside active roots per #65; no automatic import,
delete or rewrite. Hardcoded endpoint users need no override when distribution
data preserves the same endpoint; custom deployments must author their route.

## 8. Explicit decisions still open from #67

This document does not close all of the original issue. Two decisions require
separate evidence before implementation:

| Open decision | Options and evidence required |
| --- | --- |
| Gateway-discovered MCP rows | Keep only authored configured servers (this document's v1); or accept gateway rows namespaced provider/server, with static same-name entries winning and explicit mcp.trust=allow plus per-server enable required. The latter needs a real LiteLLM/Bifrost MCP listing fixture, stable server/tool identity rules, turn-boundary retirement, approval UX and two-origin captures proving gateway credentials are never implicitly reused cross-origin. Decide schema and trust interaction with #65 before adding any row. This is not revival of the retired global service directory. |
| Gateway-reported per-request monetary cost | Prefer validated reported usage cost with table fallback; or retain table-derived accounting and show reported cost separately. Need captured streaming/final usage fixtures for each adapter, currency/unit and cached-token accounting rules, ledger reconciliation, a numeric divergence margin with rationale, and TS parity evidence. Catalog price metadata precedence in section 3 does not decide per-request billing precedence. No margin or billing policy is invented here. |

## 9. Implementation handoff and evidence required later

Ownership stays within crate README scopes: pa-types owns shared schema/report
vocabulary; pa-core owns scoped config/trust/auth composition and MCP host policy;
pa-models owns discovery requests, normalized metadata, catalogs and cache;
pa-ai owns inference transport and native auth execution. CLI only wires inputs;
TUI renders daemon wire status, never links discovery internals. The Python
runtime owns MCP transport, initialize/list/call and its live memory cache;
pa-core must not spawn MCP servers (`crates/pa-core/README.md:5-20`). No new crate,
public API or dependency is approved by this document.

Implement after #65: add strict members and generated request data; resolve
origin-bound requests once; apply field provenance/order; guard cache publication
by route/account/auth generation; expose per-source and per-server outcomes.
Then add focused acceptance evidence using isolated roots and fake origins:

- Add a JSON-only provider and capture complete inference/listing requests.
- Exercise each fixed response format, defaults, missing costs, invalid/duplicate
  IDs and a route-ID collision; compare complete effective models/provenance.
- Capture two origins and assert zero redirected requests and zero inherited
  cross-origin headers, including unknown secret-bearing headers and accounts.
- Expire a token on a later page, rotate auth concurrently and verify no partial
  or old-generation publication, including after process restart/revocation.
- Test 404, empty region, offline fallback, cache write failure and hanging servers
  without delaying first paint or changing session selection.
- List disabled/unapproved/failed/empty-ready MCP servers, refresh list_changed,
  reconnect and prove stale tools are not callable; no retired directory is fetched.
- Search captured logs/cache for fixture secrets and account identifiers.

These are implementation requirements, not claims of tests run or changes to
the open suite. This PR decides the stated data/security/failure contracts only, not full issue
coverage. Gateway-discovered MCP rows and reported-cost precedence remain open
as section 8 records. It does
not decide new protocols, generic mapping languages, arbitrary OAuth providers,
xAI subscription representation, cloud SDK adoption, UI layout/daemon wire
versioning, background scheduler redesign, or closure of Week 2.5. No runtime
parity claim follows from a prose document; implementation PRs must supply the
repository's TS parity-diff evidence for their actual changed surfaces.
