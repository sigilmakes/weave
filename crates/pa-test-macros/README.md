# pa-test-macros

Test-only attribute expansion owned by the timing instrumentation. No runtime,
fixture, or production behavior belongs here. Dependencies are `syn` and `quote`;
the generated code calls `pa-test-scratch` without a dependency back to it.

Public API: `#[pa_test_scratch::pa_test]` must precede a normal `#[test]`, but
follow `#[tokio::test]`. Tokio forwards it to its synchronous wrapper, so timing
includes runtime setup and teardown instead of only the async body. It preserves
the function and attributes, inserting `_pa_test_timing` as the first local.
See `pa-test-scratch` for the recorder API.
