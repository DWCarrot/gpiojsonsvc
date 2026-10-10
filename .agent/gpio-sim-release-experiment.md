# Direct libgpiod release experiment

Date: 2026-10-09. This is a research probe, not an implementation of the planned refactor.

## Result

**Confirmed on both existing gpio-sim chips:** releasing the line request changes the physical value to the simulator's **current pull level**. Closing the chip alone does not release the line request or change its held value.

This is not a generic “restore the value from before the request” operation. A distinguishing case started low, changed the simulated pull to high through request bias configuration while driving low, and became high after release.

For physical GPIO in general, neither retaining the last output nor restoring the previous value is guaranteed after release. The kernel API guarantees the requested state only while the request fd remains open; after close the line is no longer controlled by that userspace request. It also explicitly distinguishes closing the chip fd from closing the request fd. [Kernel GPIO v2 request documentation](https://docs.kernel.org/userspace-api/gpio/gpio-v2-get-line-ioctl.html).

## Environment and method

- Kernel: `6.18.40.1-microsoft-standard-WSL2`.
- C libgpiod: `2.1.3`; constants and signatures checked against `/usr/include/gpiod.h`.
- Python `ctypes` calls directly into the shared C library; no `gpioset`, `gpioget`, or other GPIO CLI tool performs the experiment.
- Executed outside the Codex sandbox as the normal user, effective UID **1000**, with approval for host-device access. No sudo/root process or sysfs write was needed.
- Tested stable device paths for `gpiochip0` and `gpiochip1`, offset **7** on each. The probe verified simulator provenance and that each selected line was an unused input before proceeding.
- Physical values and pulls were read from gpio-sim sysfs. Ownership/direction metadata came from libgpiod. Observations after release did **not** acquire another line request, which could itself change the measured state.
- Baseline pull-up/down was prepared with a temporary input request using the C bias-setting API. The special third case configured pull-up on an output request. This is supported by this simulator; it is not an external stimulus mechanism for an unrelated service-owned line.

The core operations were:

```text
gpiod_chip_open
gpiod_line_settings_new / set_direction / set_output_value
gpiod_line_config_new / add_line_settings
gpiod_chip_request_lines
gpiod_line_request_set_value
gpiod_line_request_get_value                 # while request is valid
read gpio-sim sysfs value                   # independent observation
gpiod_line_request_release
read gpio-sim sysfs value                   # no new request
gpiod_chip_close
read gpio-sim sysfs value again, including after 100 ms
```

An additional case reversed the two close operations. After `gpiod_chip_close`, `gpiod_line_request_get_value` still worked and the line remained reserved. Only the subsequent request release changed its value and cleared ownership.

## Measurements

All four cases produced the same results on both chips: **eight cases passed**. Values below are physical, active-high levels.

| Case | Before output request | Current pull while held | After explicit set | After request release | After both handles close / 100 ms later |
| --- | --- | --- | --- | --- | --- |
| Pull-down, set high | L | Down | H | L | L / L |
| Pull-up, set low | H | Up | L | H | H / H |
| Start low, configure output pull-up, set low | L | Up | L | H | H / H |
| Pull-down, set high, close chip first | L | Down | H; still H after chip close | L | L / L |

In every case, line ownership was true while requested and false after release. The consumer name disappeared at release. Interestingly, the reported direction remained `output` after release even though the physical value followed the pull. Therefore “direction still says output” is not evidence that a request or its output value remains held.

The third case distinguishes restoration of a saved old value from following the current pull: **before=L, driven=L, released=H**. The chip-first case distinguishes chip-handle lifetime from request lifetime.

Both lines were restored using an input request with their original pull-down setting. The final sampled states exactly matched their original snapshots: `value=0`, `pull=pull-down`, `direction=input`, `used=false`, and `consumer=null`. No topology was created, reset, or removed.

## Source explanation and test consequences

The local libgpiod C source, `../libgpiod/lib/line-request.c`, implements release by closing the request fd and freeing the request object; it does not store/reapply a previous line value. `chip.c` closes the separate chip fd. The kernel simulator's `gpio_sim_free` copies the pull bitmap into the value bitmap and clears the requested flag. This directly explains the measurements. [Linux v6.18 gpio-sim source](https://raw.githubusercontent.com/torvalds/linux/v6.18/drivers/gpio/gpio-sim.c).

The `final` observation limitation is therefore experimentally confirmed, not merely inferred from CLI behavior. After final application and immediate release, a physical read sees the pull level; it cannot establish what the service wrote just before release. Retaining the chip handle alone does not help. Verify final application while the **line request** is retained, or use a separate pre-release recording mechanism if full close-path write proof is required.

These results establish this gpio-sim environment's behavior. No physical board was tested, and no guarantee about a real controller's post-release electrical state follows from them.

## Reproduction artifacts

- [Python C-ABI probe](gpio-sim-release-probe.py).
- [Raw observations and restoration checks](gpio-sim-release-probe.jsonl).

Run the saved probe as a normal user with access to the existing simulation, outside a sandbox that hides its devices:

```bash
python3 .agent/gpio-sim-release-probe.py
```

It refuses non-simulator GPIO and occupied/non-input test lines, briefly changes offset 7 on both chips, and restores their sampled state. Run it without competing users of those lines. It is an ad hoc research artifact, not yet the planned shared-lock integration harness.
