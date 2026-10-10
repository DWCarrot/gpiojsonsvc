# Replacing the mock backend tests with gpio-sim

Research date: 2026-10-09. Scope: the current working tree, including the already staged gpio-sim helper, topology, and development documentation. No service, test, configuration, or dependency implementation was changed for this research.

## Decision

**Feasibility passed for the agreed scope, with `final`-value testing explicitly deferred to TODO on 2026-10-10.** The two manually prepared chips can exercise the production libgpiod path for requests, reads, writes, metadata, ownership, grouping, and protocol/session behavior. Pure logic tests should remain independent of devices. Tests of the deleted XML simulator itself should be retired.

**Point 1 resolved by the user:** when `initial` is omitted from `init`, the output value is unspecified. Tests must not assert its value until the first explicit `set` for that pin. Explicit `initial` values retain their existing guarantees.

**Point 2 resolved as a test-workflow decision:** tests that inject input transitions will be run manually with root permission. Non-root runs report these cases as ignored through a runtime permission check; no broker or permission-grant installation is required.

**Point 3 resolved for ordinary operations and stepped set:** use concurrent physical-value observations, preferably 500 ms between steps, and no precise wall-clock timing assertions. This checks the resulting values of each observable step; exact low-level write counts are outside this acceptance criterion.

**Point 3 closed for this refactor:** the user has deferred `final`-value tests to [TODO](../docs/TODO). The observation limitation remains real, but no longer blocks the refactor. Keep production final-write behavior and ordinary teardown/release tests; omit final-value assertions from the migrated suite for now.

The [architecture proposal](gpio-sim-architecture.md) is viable with the accepted initial, permission, stepped-set, and final-test deferral policies. Feasibility approval is not a claim that the converted suite has been implemented or run. Deferred coverage must be reported explicitly.

## Evidence and current environment

- Read [the development workflow](../docs/gpio_sim_development.md), the helper and topology, active Rust test modules, smoke tests, Python tests, protocol documentation, and both GPIO implementations. Archived Rust modules are not compiled and are excluded from test counts.
- `cargo test -- --list`: **214 unit tests and 13 integration smoke tests**.
- Baseline `cargo test`: **214 + 13 passed**, zero ignored, outside the sandbox. This establishes the old suite's health; it does not validate a converted suite. Log: `/tmp/gpiojsonsvc-query-tests.log`.
- `python3 tools/test_gpio_sim_dev.py`: **4 passed**. These cover `gpioinfo` parsing, not real simulator operations.
- `python3 tools/test_debug_client.py`: **51 passed** outside the sandbox. The first sandbox run encountered four Unix-socket permission errors. Log: `/tmp/gpiojsonsvc-query-python-tests.log`.
- `status`, `get`, and approved read-only `dump ... --format json` inspection confirmed `gpiochip0` and `gpiochip1`, eight lines each, initially low/input with no reported consumers. Stable paths are `/run/gpiojsonsvc-gpio-sim/gpiochip0` and `/run/gpiojsonsvc-gpio-sim/gpiochip1`. Current kernel allocation happens to match these names; tests must not depend on that.
- Host kernel reports `6.18.40.1-microsoft-standard-WSL2`; pkg-config reports libgpiod `2.1.3`. The sandbox exposes simulator sysfs reads but hides its character devices. `dump` succeeded after running outside the sandbox as the normal user.
- At the initial research stage, no GPIO write experiment was performed. A subsequent user-requested [direct C-ABI release experiment](gpio-sim-release-experiment.md) passed eight cases on the existing chips as UID 1000 and restored both tested lines. It confirmed return to current pull on request release and retention when only the chip closes. No topology creation/reset or external sysfs pull injection was performed; other runtime characterization remains required during implementation.
- Follow-up research verified runtime root-gating support in the `test-with` macro source and runtime ignore support in `libtest-mimic`. Stepped-set feasibility was checked against the reactor's operation/completion order. No new framework dependency was installed and no privileged test was run for this follow-up.

## Inventory summary

“Audited” includes pure tests in affected modules so that removal of a module does not accidentally remove useful coverage. It is not a count of tests that must all become device tests.

| Location | Existing tests | Disposition |
| --- | ---: | --- |
| `src/gpio/mock/{mod,chip,config,events,log,snapshot,state}.rs` | 63 | Retire implementation-only cases; move useful GPIO semantics to the concrete wrapper suite. |
| `src/app.rs` | 18 | Remove mock selection/XML startup cases; retain CLI/socket cases and adapt startup validation. |
| `src/session/initialized.rs` | 17 | Retain validation/configuration tests; convert requests and initial tests; defer dedicated final tests. |
| `src/session/execute.rs` | 6 | Separate compile-only checks from device reads/writes. |
| `src/session/query.rs` | 2 | Convert to kernel metadata and non-mutating observations. |
| `src/session/reactor.rs` | 15 | Convert live GPIO cases; preserve device-free state/lifecycle checks where possible. |
| `tests/smoke.rs` | 13 | Replace process harness and observations; retire mock CLI/log features. |
| **Affected-module inventory** | **134** | Full named inventory below. |
| Other active Rust modules | 93 | Retain; includes 20 GPIO type/wrapper tests, three of which already probe devices. |

The remaining 93 comprise config (27), protocol (29), session batch/compiled/sequence (11), system events (3), transport (3), GPIO values (2), system conversion (8), and system wrapper (10). They need no blanket conversion. The system wrapper's `first_gpiochip()` scans `/dev/gpiochip0..7` and silently returns when unavailable; replace that discovery for its three device cases with verified simulation discovery and real skip reporting.

Python's `unittest.mock` usage is client-side isolation, not `gpio::mock`; retain all 51 debug-client tests. Retain the four helper-parser tests: `test_parses_requested_input`, `test_normalizes_flags_keys_and_quoted_values`, `test_rejects_missing_direction`, and `test_rejects_duplicate_attributes`.

## Coverage and limitations

### Ordinary GPIO behavior: supported

Use libgpiod requests for configuration and I/O; inspect physical state through gpio-sim sysfs `value`, directly or through helper `get`. This is independent of the request owner's logical readback and works while a line is reserved. Use `get_line_info` or `gpioinfo` for consumer, direction, and ownership. Do not use `gpioget`, `gpioset`, or `gpiomon` as an observer of a line already reserved by the service: another line request conflicts with exclusive ownership. The simulator exposes the normal GPIO character-device API and separate sysfs controls. [Kernel simulator documentation](https://cdn.kernel.org/doc/html/latest/admin-guide/gpio/gpio-sim.html).

The fixed 2 × 8 topology is sufficient for current behavior. Remap XML fixture offset **13** to chip1 offset **5**, while keeping opaque protocol keys such as `GPIO1_B5` and independent IDs such as `26`. Keep out-of-range tests on offset 99. Do not infer offsets from names, and do not enlarge/recreate the user's chips just to retain obsolete fixture offsets.

### Omitted initial values: decision resolved

`MockLineSettings` separately remembers whether an output value was configured; `request_keeps_stored_output_level_without_output_value` and session/reactor tests depend on it. In contrast, libgpiod settings reset to logical inactive. `InitializedSession::initialize` sets output direction and only overrides that default when `initial` is present. Therefore changing the backend is not sufficient to preserve the README/protocol promise. The local libgpiod source at `../libgpiod/lib/line-settings.c`, `gpiod_line_settings_reset`, confirms the default; upstream also documents inactive as the settings default. [libgpiod line settings](https://libgpiod.readthedocs.io/en/master/python_line_settings.html).

The user has specified the replacement contract: **omitting `initial` makes no guarantee about the output value**. Neither preservation nor logical inactive is promised. The libgpiod default is an implementation detail and must not become a service-level assertion.

During implementation, correct the README/protocol promise and replace preservation tests with successful initialization followed by an explicit `set`. Assert the output only after that set has been applied, and only for pins explicitly set. Do not inspect unspecified output values between init and the first set. Tests needing a known value immediately after init must supply `initial`. Explicit initial-value tests remain valid. No preservation mechanism or forced default-value contract is needed; this point no longer blocks feasibility.

### Final writes: tests deferred to TODO (2026-10-10)

The Linux driver’s `gpio_sim_free` restores `value_map` from `pull_map`; `gpio_sim_set_config` applies pull-up/down to simulator state. Thus post-release low can occur even when `final: 0` was never applied, and a successful `final: 1` can disappear before polling sees it. This also means input bias affects actual levels, unlike the XML fixture's metadata-only bias behavior. [Linux v6.18 gpio-sim source](https://raw.githubusercontent.com/torvalds/linux/v6.18/drivers/gpio/gpio-sim.c).

This is now verified using Python `ctypes` and direct libgpiod calls on both chips. The [experiment](gpio-sim-release-experiment.md) observed L→H→L with pull-down and H→L→H with pull-up. Changing the output request's bias demonstrated that release follows the **current pull**, not a saved pre-request value. Closing only the chip kept the request/value alive; releasing the request changed the value even with the chip still open.

The user has chosen to defer dedicated `final`-value test migration and additions, including final-batch-specific cases, to [TODO](../docs/TODO). Do not introduce a final-application test seam, tracing facility, or teardown delay as a requirement of this refactor. Existing protocol parsing/validation tests can remain.

Keep smoke assertions for clean disconnect/SIGINT, socket removal, ownership release, and successful re-acquisition. Split mixed cases so their non-final assertions still run. If a final-only case remains registered, mark it explicitly ignored with a TODO reason; if its mock-dependent implementation is removed, retain its deferred disposition in this inventory. Neither treatment counts as passing final-value coverage.

When the TODO is resumed, consider checking final-batch compilation and physical application while the request is retained, then separately choose a pre-release recorder if full close-path proof is required. These are future options, not current acceptance gates.

### Ordinary operations and stepped set: concurrent checks are feasible

The user's suggested 200–500 ms step intervals solve the observation problem for ordinary stepped-set tests. Prefer **500 ms initially**, with a separate observer thread (or a task that cannot be blocked by the reactor). Exact execution times are not asserted. The current `handle_set_sequence` applies the first step before returning to its loop; the correlated `ok` is sent only after the last step. Therefore the observer must start before sending the request, not after awaiting its reply.

Concrete two-step procedure, using an output with explicit `initial: 0`:

1. After init succeeds, verify physical L and prepare an observer of the verified sysfs `value` path. Synchronize observer readiness before sending the stepped set.
2. Send a single sequence: step 0 sets 1, step 1 sets 0 with `lag: 500`. Leave the service connection open.
3. The observer polls, for example every 10–20 ms, and records physical H followed by L. The first successful H observation verifies step 0; this avoids relying on a single precisely timed read.
4. Around/after 500 ms, continue polling with a generous overall deadline (for example 3 seconds), and await the correlated completion reply. Assert physical L after `ok`, while the service still owns the line, and require the observer to have recorded H before L.
5. Join the observer and close the client only after both value checks and completion have succeeded. On timeout, fail with observations/diagnostics and clean up; do not report a skip.

Checking once at exactly 500 ms can race with the service's second step. Polling plus the completion reply provides better synchronization without measuring timing accuracy. Use direct sysfs reads for repeated sampling; launching Python for every 10 ms poll adds unnecessary overhead. `tools/gpio_sim_dev.py get` remains suitable for individual checks. Do not use a second line request or protocol `get` on an output pin as the observer.

For longer sequences, choose distinct adjacent output patterns and collect each in order; keep the last value stable until verification completes. For immediate set or explicit initial, check after its correlated `ok`. For a rejected operation, use a previously known value and verify it is unchanged. Omitted initial remains unspecified until the first explicit set.

This is practical functional coverage, not a mathematical guarantee against an arbitrarily descheduled observer: even a separate thread can miss a whole plateau under severe host load. Prefer larger test lags if necessary; retain a bounded deadline. Repeated identical writes cannot be distinguished by value polling, and multi-chip snapshots are not atomic. Neither precise timing nor exact write counts are required by the user's revised test goal. Keep pure sequence compilation/order tests. No tracing facility is needed for this stepped-set plan.

The plan does **not** cover close-time `final` by waiting 500+ ms: `finish_close` writes final and immediately drops the request in the same path. There is no retained plateau to sample. The user has deferred that coverage to TODO; no additional final observer is required now.

### Input edges: manually run root-only tests

The current helper resolves an exact configured pin and writes its sysfs `pull`; `set_level()` explicitly requires root. The existing group grant covers the device node, not those controls. `libgpiosim` would still encounter the same permission boundary, and topology creation through it would conflict with the requested manual lifecycle.

**Accepted workflow:** run input-injection cases manually as root; automatically ignore them for a normal user. This supersedes the earlier broker/permission-grant recommendation. The root test process may call the existing helper's `set` command or write the verified simulator pull controls directly. It must still use the user's existing topology and restore modified pulls; it must not start/reset/stop chips.

Framework support is available:

| Option | Permission check | Suitability |
| --- | --- | --- |
| `test-with` runtime mode | `#[test_with::runtime_root()]`, with runtime and user features and its custom runner/module arrangement | Suitable; root is checked when the test runs. |
| Ordinary `#[test_with::root()]` | Condition is evaluated during macro expansion/build | Unsuitable for build-as-user, execute-as-root; the build-time decision can remain stale. |
| Proposed `libtest-mimic` runner | Check `nix::unistd::geteuid().is_root()` in the `Trial::ignorable_test` callback, returning an ignored completion with a reason | Recommended with the existing proposal; combines root, topology, and actual control access checks. |

The macro implementations distinguish ordinary and runtime root checks. [test-with root macro source](https://docs.rs/test-with-derive/latest/src/test_with_derive/lib.rs.html). The alternative supports runtime ignored results with reasons. [libtest-mimic Trial API](https://docs.rs/libtest-mimic/latest/libtest_mimic/struct.Trial.html). `nix::unistd::geteuid` requires its `user` feature; the repository's cached nix 0.30 source also confirms this. [nix effective UID API](https://docs.rs/nix/latest/nix/unistd/fn.geteuid.html).

Build test executables as the normal user with `cargo test --no-run`; manually execute the exact resulting GPIO test executable with sudo and an input-test name filter. No `sudo cargo test`, automatic elevation, or broker installation is necessary. Cargo supports compiling without executing. [Cargo test options](https://doc.rust-lang.org/cargo/commands/cargo-test.html). Use runtime gating so the same compiled case is ignored as a user and executes under sudo. This is a proposed workflow for the new runner, not a command for root-running the current unconverted suite.

Effective UID 0 is necessary for this chosen policy but insufficient by itself: verify the simulator and that its pull controls can actually be opened for writing, without changing levels during discovery. A container may expose a read-only sysfs even to root. Missing privileges/devices produce a named ignore in ordinary mode; an explicitly required manual root run must fail if its requested cases cannot execute. Once a test starts, unexpected write errors fail it. Permission checking must not depend on `$USER` or `$SUDO_USER`.

Classify basic-device availability separately from root-only input injection. Normal non-root runs still exercise all basic cases; a manually selected root run requires the injection cases. Set the baseline before requesting edges, await successful init, inject one transition, consume/verify the event, then inject the next. gpio-sim is not a physical wire between different lines: driving a spare output does not stimulate another input automatically.

### Other differences to account for

| Existing assumption | Replacement |
| --- | --- |
| Sparse XML offsets and arbitrary chip IDs/labels | Dense offsets 0–7; compare metadata against discovered topology. Kernel chip names may vary. |
| `pull_up` metadata can coexist with physical L indefinitely | Establish inputs deliberately; gpio-sim bias can change pull/value. The smoke mixed-input case currently expects `[0,1,0]` despite pull-up on its first input; rewrite fixture setup and expected levels coherently. |
| Partial/extra `set_output_values` are accepted | Current `SysLineConfig` requires an exact length and returns `LengthMismatch`. Preserve that concrete-wrapper contract; retire permissive mock assertions. |
| Reading an empty edge queue returns a mock string error | Real reads can block. Use a bounded wait/read pair; after draining, assert `wait_edge_events(Some(0)) == Timeout`. |
| Requests conflict only within one mock instance | Kernel reservations work across handles, sessions, and processes. Check typed errno/ownership rather than mock message substrings. |
| Requesting offset 99 yields `InvalidOffset` in mock | `SysChip::request_lines` currently propagates errno; session missing-line mapping expects `InvalidOffset`. Validate requested offsets explicitly if preserving the typed session error. |
| Query leaves XML bytes/log unchanged | Compare values and fresh metadata, and optionally watch line-info events to ensure query does not acquire/reconfigure lines. |
| Query test name says it resolves all names before I/O | Its actual assertions accept an I/O error for `[A, UNKNOWN]` when A has a missing device; current implementation resolves/opens incrementally. Preserve actual behavior or propose a separate change. |
| Polling watcher and per-backend cache lifetime | Delete these implementation assertions; kernel handles and RAII own lifetime. |

Also remove the reactor's mock-specific `"no edge events"` error-string branch. The current watcher can enqueue readiness notifications before the reactor drains the fd. Guard the real read with a zero-time wait so a duplicate/stale notification cannot block a Tokio worker; test this case explicitly.

## Test skipping and shared devices

Ordinary `test-with` attributes perform environment-dependent decisions during compilation; they can become stale when the simulator is started/stopped without rebuilding. Its runtime mode uses a custom runner and supports an ignore reason. [test-with documentation](https://docs.rs/test-with/latest/test_with/).

Recommendation: use a separate `harness = false` GPIO test target with **libtest-mimic 0.8.2 or a compatible pinned release**, `Trial::ignorable_test`, and a Tokio runtime for async cases. This offers runtime ignored results with reasons directly, without adding a runtime testing dependency to the service. Keep ordinary pure unit tests on Rust's default harness. `test-with` runtime mode is also viable; do not simply stack a compile-time path attribute on every `#[tokio::test]`. [libtest-mimic Trial API](https://docs.rs/libtest-mimic/latest/libtest_mimic/struct.Trial.html).

Preflight checks must verify both stable links, their real character-device identities and gpio-sim provenance, expected line counts/names, read/write device access, and readable `value` controls. A loaded module alone is insufficient, and requiring a loadable module would incorrectly exclude a built-in driver. Follow the existing development document's restriction against GPIO-dependent tests on machines with hardware GPIO controllers.

Distinguish missing prerequisites (ignored in normal developer mode) from failures after a prepared fixture has been acquired (test failure). A strict CI option, proposed as `GPIOJSONSVC_REQUIRE_GPIO_SIM=1`, makes unavailable basic devices fail; a separate required-input-injection setting prevents permanent edge-test skipping from producing a misleading green build. Wrong topology, unexpected busy lines, or permission regression in a required environment should fail with diagnostics.

All tests share the manually created topology. Use one cross-process advisory lock for both chips, held through request cleanup and child-process reaping. Root and normal-user runs must use the **same topology-scoped lock**, not separate locks keyed by effective UID; make the lock accessible to both. An in-process mutex, `serial_test::serial`, or `--test-threads=1` alone does not coordinate separate test binaries/Cargo invocations. Bound lock acquisition, avoid holding a blocking lock operation on a Tokio worker, and do not kill unrelated owners. Tests never start/reset/stop the topology. Restore relevant pull state when injection is available; explicitly initialize output levels and input bias instead of assuming earlier tests left a pristine chip.

## Suggested implementation acceptance gates

1. Apply the recorded omitted-initial decision: update documentation, remove preservation/default-level assertions, and check output values only after explicit initial or set operations.
2. Characterize bias, active-low, missing offsets, release behavior, and stale readiness on the supported host. No omitted-initial output value is an acceptance criterion.
3. Demonstrate basic tests execute, and absent devices report **ignored**, using the same already-built test executable before/after manual preparation.
4. Demonstrate that the same built edge-test executable reports root-required ignores as a normal user and passes its selected cases when manually run as root, including filtering, correlation ID, and no duplicate events. Require actual simulator/pull access too.
5. Replace every retained behavior in the inventory below; record retired mock implementation assertions separately from lost service coverage.
6. Demonstrate intermediate and last set values using the concurrent 500 ms plan without tight timing assertions. Keep teardown/release checks and explicitly report `final`-value coverage as deferred to TODO; it is not an acceptance gate.
7. Re-run pure tests, simulator integration, smoke, and both Python suites. A passing old mock suite is not acceptance for the new architecture.

## Named inventory

Legend: **P** retain as a device-free test; **G** convert to a verified gpio-sim test; **E** needs input injection and is manually run as root (runtime ignore otherwise); **C** requires a semantic/observation change described above; **D** retire with the removed feature; **T** defer to [TODO](../docs/TODO) by user decision. A combined label means split the existing test's assertions. File headings identify the source; names are exact.

### `src/gpio/mock/chip.rs` (1)

| Test | Action | Replacement / reason |
| --- | --- | --- |
| `concurrent_opens_share_one_state_and_watcher_and_release_ownership` | G/D | Check exclusivity/release across independent handles; retire shared-cache/watcher counts. |

### `src/gpio/mock/config.rs` (4)

| Test | Action | Replacement / reason |
| --- | --- | --- |
| `line_config_preserves_assignment_order` | P | Move to concrete libgpiod config test; no chip required. |
| `default_output_value_is_unset` | D | Retire mock unset-state assertion; do not replace it with a guaranteed omitted-initial output value. |
| `set_output_values_accepts_partial_and_extra_lengths` | C/P | Concrete wrapper rejects non-exact lengths; test LengthMismatch. |
| `duplicate_offset_last_mapping_wins` | P | Check replacement settings and stable offset order on concrete config. |

### `src/gpio/mock/events.rs` (2)

| Test | Action | Replacement / reason |
| --- | --- | --- |
| `edge_event_buffer_respects_capacity` | P/E | Retire mock push_record; test wrapper capacity guards and real reads separately. |
| `default_buffer_capacity_matches_libgpiod_default` | P | Merge into existing system buffer tests; test requested capacities too. |

### `src/gpio/mock/log.rs` (2)

| Test | Action | Replacement / reason |
| --- | --- | --- |
| `dump_block_format_matches_plan` | D | XML parsing/serialization, reload/cache, or mock log implementation disappears; no simulator equivalent is required. |
| `enqueue_and_flush_appends_to_file` | D | XML parsing/serialization, reload/cache, or mock log implementation disappears; no simulator equivalent is required. |

### `src/gpio/mock/mod.rs` (36)

| Test | Action | Replacement / reason |
| --- | --- | --- |
| `parse_and_round_trip_sample_chip` | D | XML parsing/serialization, reload/cache, or mock log implementation disappears; no simulator equivalent is required. |
| `reject_duplicate_line_ids` | D | XML parsing/serialization, reload/cache, or mock log implementation disappears; no simulator equivalent is required. |
| `reject_input_line_with_drive` | D | XML parsing/serialization, reload/cache, or mock log implementation disappears; no simulator equivalent is required. |
| `reject_output_line_with_bias` | D | XML parsing/serialization, reload/cache, or mock log implementation disappears; no simulator equivalent is required. |
| `reject_invalid_level_text` | D | XML parsing/serialization, reload/cache, or mock log implementation disappears; no simulator equivalent is required. |
| `reject_empty_chip_id` | D | XML parsing/serialization, reload/cache, or mock log implementation disappears; no simulator equivalent is required. |
| `reject_unknown_root_element` | D | XML parsing/serialization, reload/cache, or mock log implementation disappears; no simulator equivalent is required. |
| `reject_unknown_line_attribute` | D | XML parsing/serialization, reload/cache, or mock log implementation disappears; no simulator equivalent is required. |
| `is_gpiochip_device_rejects_invalid_files` | P | Use concrete device check on regular/missing paths; merge existing coverage. |
| `normalize_missing_optional_attributes` | D | XML parsing/serialization, reload/cache, or mock log implementation disappears; no simulator equivalent is required. |
| `open_chip_exposes_metadata_and_path` | G | Assert discovered path/name/label and eight lines. |
| `request_read_write_and_persist_output` | G/C | Explicit initial value, request readback and sysfs physical level; remove XML persistence. |
| `request_lines_materializes_per_line_output_value_without_set_output_values` | G | Observe physical output while request is alive. |
| `set_output_values_override_per_line_output_value_defaults` | G | Keep per-line versus bulk override precedence, exact-sized values. |
| `request_keeps_stored_output_level_without_output_value` | G/C | Replace preservation assertion with successful request, explicit set, then value verification; do not inspect the value before set. |
| `partial_set_output_values_falls_back_to_per_line_defaults` | C/P | Replace permissive mock behavior with concrete length-validation coverage. |
| `active_low_inverts_materialized_output_default` | G | Compare logical active against physical low while held. |
| `set_values_subset_persists_output_levels` | G | Keep reordered subset read/write and physical-level assertions. |
| `line_info_reports_consumer_while_requested` | G | Inspect fresh kernel metadata before/during/after ownership. |
| `overlapping_requests_are_rejected` | G | Check busy errno; no mock error-string comparison. |
| `partial_overlap_requests_are_rejected` | G | Check busy failure and that non-overlapping lines were not leaked. |
| `dropped_request_releases_exclusive_lines` | G | Release, inspect unused status, and re-request. |
| `empty_line_config_is_rejected` | G | Merge system wrapper case using verified simulator discovery. |
| `get_requested_offsets_follows_line_config_order` | G | Keep request offset ordering. |
| `request_lines_persists_property_changes_without_output_values` | G/C | Check live input/bias metadata and actual bias effect; remove disk assertion. |
| `active_low_inverts_logical_values` | G | Prepare a known input with explicit bias and check logical inversion. |
| `external_file_reload_ignores_metadata_and_active_low_edits` | D | XML parsing/serialization, reload/cache, or mock log implementation disappears; no simulator equivalent is required. |
| `active_low_xml_load_and_persist_round_trip` | D | XML parsing/serialization, reload/cache, or mock log implementation disappears; no simulator equivalent is required. |
| `read_edge_events_errors_when_queue_empty` | C/G | Assert bounded wait timeout; never perform a blocking empty read. |
| `external_input_change_produces_edge_event` | E | Inject pull, wait, read one event, then verify queue drained. |
| `edge_detection_filters_transitions` | E | Drive falling/rising transitions with acknowledgements; assert filter and offset. |
| `eventfd_is_readable_when_events_queued_and_drained` | E | Poll real request fd before and after drain; no eventfd dependency. |
| `write_log_disabled_by_default` | D | XML parsing/serialization, reload/cache, or mock log implementation disappears; no simulator equivalent is required. |
| `write_log_records_set_value_and_set_values_subset` | D | XML parsing/serialization, reload/cache, or mock log implementation disappears; no simulator equivalent is required. |
| `write_log_records_request_time_output_value` | D | XML parsing/serialization, reload/cache, or mock log implementation disappears; no simulator equivalent is required. |
| `write_log_skips_request_and_drop_persist` | D | XML parsing/serialization, reload/cache, or mock log implementation disappears; no simulator equivalent is required. |

### `src/gpio/mock/snapshot.rs` (13)

| Test | Action | Replacement / reason |
| --- | --- | --- |
| `load_save_round_trip_preserves_normalized_snapshot` | D | XML parsing/serialization, reload/cache, or mock log implementation disappears; no simulator equivalent is required. |
| `load_snapshot_rejects_gpiochips_wrapper` | D | XML parsing/serialization, reload/cache, or mock log implementation disappears; no simulator equivalent is required. |
| `load_snapshot_rejects_a_second_gpiochip` | D | XML parsing/serialization, reload/cache, or mock log implementation disappears; no simulator equivalent is required. |
| `load_snapshot_keeps_chip_id_as_metadata` | D | XML parsing/serialization, reload/cache, or mock log implementation disappears; no simulator equivalent is required. |
| `diff_snapshot_rejects_gpiochips_wrapper` | D | XML parsing/serialization, reload/cache, or mock log implementation disappears; no simulator equivalent is required. |
| `load_save_round_trip_through_reader_writer_handles` | D | XML parsing/serialization, reload/cache, or mock log implementation disappears; no simulator equivalent is required. |
| `load_snapshot_stores_physical_levels_without_active_low_conversion` | D | XML parsing/serialization, reload/cache, or mock log implementation disappears; no simulator equivalent is required. |
| `save_snapshot_writes_physical_levels_directly` | D | XML parsing/serialization, reload/cache, or mock log implementation disappears; no simulator equivalent is required. |
| `diff_snapshot_reports_only_changed_input_values` | D | XML parsing/serialization, reload/cache, or mock log implementation disappears; no simulator equivalent is required. |
| `diff_snapshot_ignores_metadata_only_edits` | D | XML parsing/serialization, reload/cache, or mock log implementation disappears; no simulator equivalent is required. |
| `diff_snapshot_converts_physical_changes_with_baseline_active_low` | D | XML parsing/serialization, reload/cache, or mock log implementation disappears; no simulator equivalent is required. |
| `line_level_conversion_respects_active_low` | G | Move physical/logical behavior to concrete wrapper tests; remove mock helper. |
| `active_low_xml_round_trip_preserves_attribute_and_levels` | D | XML parsing/serialization, reload/cache, or mock log implementation disappears; no simulator equivalent is required. |

### `src/gpio/mock/state.rs` (5)

| Test | Action | Replacement / reason |
| --- | --- | --- |
| `external_file_diff_enqueues_matching_input_transition` | E/D | Merge edge assertions into real event tests; delete file-diff machinery. |
| `external_file_diff_respects_edge_detection_filter` | E/D | Merge real filtering coverage; delete file-diff machinery. |
| `external_file_diff_ignores_output_line_changes` | E/D | Optional simulator check: external pull cannot override a held push-pull output; no XML diff. |
| `external_file_diff_ignores_metadata_only_changes` | D | XML parsing/serialization, reload/cache, or mock log implementation disappears; no simulator equivalent is required. |
| `external_file_diff_records_watcher_error_on_unknown_line` | D | XML parsing/serialization, reload/cache, or mock log implementation disappears; no simulator equivalent is required. |

### `src/app.rs` (18)

| Test | Action | Replacement / reason |
| --- | --- | --- |
| `cli_parses_mock_and_config_path` | P/C | Retain positional config parsing; add explicit rejection of removed --mock. |
| `cli_parses_config_path_then_mock` | D | Remove obsolete flag-order combination; config ordering remains covered. |
| `cli_parses_mock_without_config_path` | D | Remove obsolete flag acceptance. |
| `cli_defaults_to_real_backend_without_mock` | P | Retain default config discovery; no selector remains. |
| `cli_rejects_unknown_option` | P | Retain and include --mock rejection. |
| `cli_rejects_extra_positional` | P | Retain unchanged. |
| `cli_rejects_duplicate_mock` | D | No duplicate flag state after option removal. |
| `cli_help_and_version_short_circuit` | P | Retain; update help expectations. |
| `mock_mode_uses_env_log_path` | D | Delete MockMode/environment-log feature tests. |
| `select_backend_off_selects_real_backend` | D | No backend selection function in final design. |
| `select_backend_enables_write_log_when_path_is_given` | D | Delete removed logging feature. |
| `select_backend_rejects_unwritable_write_log_path` | D | Delete removed logging feature. |
| `run_without_mock_rejects_non_gpiochip_device` | P | Retain ordinary-file rejection before binding. |
| `mock_startup_rejects_missing_chip_file` | P/C | Retain missing-device failure using concrete startup validation. |
| `mock_startup_rejects_invalid_chip_xml` | P/D | Delete XML parse case; ordinary file remains invalid as a GPIO device. |
| `mock_startup_rejects_unavailable_line` | G/C | Preserve startup line validation through chip metadata; current real startup checks only device kind. |
| `serve_binds_then_removes_socket_on_shutdown` | P | Direct runtime socket lifecycle needs no GPIO request or mock object. |
| `serve_closes_connected_clients_on_shutdown` | P | Uninitialized-client shutdown can remain device-free. |

### `src/session/initialized.rs` (17)

| Test | Action | Replacement / reason |
| --- | --- | --- |
| `initialize_classifies_invalid_request_parameters` | P | Concrete settings allocation is device-free; invalid input precedes chip I/O. |
| `initialize_opens_chip_and_requests_configured_lines` | G/C | Preserve grouping/consumer/read assertions; explicitly seed output previously inherited from XML. |
| `initialize_opens_two_configured_device_paths_as_independent_chips` | G | Use chip0 and chip1; map the old offset 13 to 5. |
| `pin_map_session_config_renders_default_consumer_template` | P | Keep consumer-template test unchanged. |
| `service_config_renders_configured_gpio_consumer` | P | Keep config rendering test unchanged. |
| `initialize_applies_custom_consumer_to_requested_lines` | G | Assert fresh kernel consumer metadata while owned. |
| `initialize_reuses_one_session_chip_for_pins_sharing_a_device_path` | G | Keep same-device grouping and offset assertions. |
| `initialize_rejects_unmapped_pin` | P | Keep exact-key error before chip I/O. |
| `initialize_rejects_unavailable_device_file` | P | Use nonexistent device path; no prepared simulator necessary. |
| `initialize_rejects_missing_line` | G/C | Offset 99 on eight-line chip; normalize to typed MissingLine explicitly. |
| `initialize_rejects_duplicate_physical_location_in_combined_target` | P | Compile duplicate physical mapping before opening devices. |
| `initialize_applies_single_initial_value` | G | Check held initial output; dedicated final-batch assertions are deferred. |
| `initialize_broadcasts_combined_initial_value` | G | Check both held outputs with explicit initial. |
| `initialize_preserves_persisted_output_when_initial_is_omitted` | G/C | Rename to cover omitted-initial init followed by explicit set; verify values only after set. Contract decision resolved. |
| `initialize_compiles_retained_final_batch` | T | Defer dedicated final-batch test migration to TODO. |
| `initialize_broadcasts_combined_final_value` | T | Defer dedicated final-broadcast test migration to TODO. |
| `initialize_rejects_overlapping_init_entries` | P | Keep pre-I/O duplicate detection; no mock fixture needed. |

### `src/session/execute.rs` (6)

| Test | Action | Replacement / reason |
| --- | --- | --- |
| `reads_single_and_multiple_pins_in_request_order` | G | Prepare deterministic input levels/bias and preserve read ordering. |
| `writes_independent_pin_values_in_one_batch` | G | Check independent output readback and sysfs values. |
| `rejects_combined_names_unknown_pins_and_wrong_modes` | P | Build CompiledPins directly instead of sample_session(). |
| `rejects_non_bit_values_before_applying_any_writes` | P/G | Pure rejection plus live unchanged-output regression test. |
| `adding_individual_pins_preserves_read_slots_and_write_conflict_checks` | P/G | Pure duplicate/conflict compilation plus live duplicate read-slot check. |
| `batch_error_retains_typed_source` | P | Retain unchanged. |

### `src/session/query.rs` (2)

| Test | Action | Replacement / reason |
| --- | --- | --- |
| `queries_selected_opaque_keys_aliases_and_multiple_chips_without_writes` | G/C | Use exact keys/IDs, metadata and value snapshots; set output direction intentionally, no XML/log oracle. |
| `resolves_all_names_before_io_and_does_not_open_unselected_devices` | P/G | Keep actual incremental error precedence; rename misleading test and retain valid/unselected-device cases. |

### `src/session/reactor.rs` (15)

| Test | Action | Replacement / reason |
| --- | --- | --- |
| `query_during_set_sequence_does_not_change_sequence_or_state` | G | Observe held values and normal completion across metadata queries; no XML equality. |
| `get_before_init_returns_not_initialized_then_init_succeeds` | P/G | Device-free pre-init error plus concrete successful init. |
| `init_get_immediate_set_round_trip` | G | Real session/request and physical output observation. |
| `second_init_is_rejected` | G | Keep original ownership and correlated rejection. |
| `set_with_steps_runs_to_completion` | G/C | Observe ordered values concurrently with 500 ms lags; final check after ok while held. No exact timing/write-count assertion required. |
| `set_while_sequence_running_is_rejected` | G | Keep long-running sequence, correlated rejection, and final completion. |
| `trigger_edge_emits_event_with_init_request_id` | E | Inject after init succeeds; verify target, edge kind, and init ID. |
| `init_applies_single_initial_value` | G | Read physical output while reactor owns request. |
| `init_applies_broadcast_combined_initial_value` | G | Read both physical outputs while owned. |
| `init_omitting_initial_preserves_persisted_output` | G/C | Rename to cover omitted-initial init followed by explicit set; verify values only after set. Contract decision resolved. |
| `disconnect_applies_broadcast_final_output_values` | T/G | Defer final-value assertions; retain disconnect completion and ownership release. |
| `transport_closed_ends_the_reactor_task` | G | Keep initialized shutdown case and bounded completion/release. |
| `shutdown_event_ends_the_reactor_task` | P | Current test never initializes GPIO; remove unused XML fixture. |
| `service_shutdown_applies_final_output_values` | T/G | Defer final-value assertions; retain service-shutdown completion and ownership release. |
| `deferred_commands_do_not_panic_or_emit_responses` | P/G | Split pre-init no-op checks from successful concrete init; add stale-readiness regression. |

### `tests/smoke.rs` (13)

| Test | Action | Replacement / reason |
| --- | --- | --- |
| `query_filters_reports_other_sessions_and_releases_ownership` | G/T | Keep filters/IDs/ownership/conflict/retry and non-mutating query checks; defer post-release final-value assertion. |
| `cli_without_mock_fails_before_binding` | P | Rename to invalid_device_fails_before_binding; use regular file. |
| `cli_mock_flag_is_required_even_with_env_config` | P/C | Retain environment config discovery and invalid-device rejection; remove obsolete flag requirement. |
| `cli_mock_log_env_without_mock_fails` | D | Retire removed environment feature (or replace only if a deprecation policy is chosen). |
| `cli_help_documents_mock_and_config` | P/C | Help must document config and omit removed flag/log variable. |
| `cli_mock_with_log_path_records_writes` | D | Retire write-log feature; output behavior covered by live smoke cases. |
| `uds_init_get_immediate_set_and_stepped_set_persist_mock_state` | G/C | Replace XML with sysfs observations while alive; observer ready before send, 500 ms lags, and last value after ok. |
| `uds_unmapped_pin_returns_correlated_error` | G | Run normal executable against temporary simulation config; verify correlation. |
| `uds_init_initial_final_and_omitted_values` | G/T | Check explicit initial and values after set for omitted initial; retain release checks, defer final-value assertions. |
| `sigint_applies_final_output_values` | T/G | Defer final-value assertion; retain graceful SIGINT exit and ownership release. |
| `sigint_exits_while_a_client_is_connected` | G | Keep graceful exit and socket removal; assert ownership released. |
| `separate_cross_chip_writes_and_invalid_later_step` | G/T | Keep held-output and validation-before-write assertions; defer final-after-drop value assertion. |
| `init_group_allows_individual_and_reordered_cross_chip_reads` | G/C | Remap chip1 offset; replace XML level edit and contradictory pull-up expectations with explicit setup. |
