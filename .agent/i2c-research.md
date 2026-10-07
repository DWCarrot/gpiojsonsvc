# I²C facts and comparison with i2c-tools

Research date: 2026-10-07. Sources below are upstream tool documentation/code,
Linux documentation/UAPI, and library documentation. `master`/`latest` links
can change; implementation must also check the deployed kernel and tool version.

## Transfers, messages, and registers

A normal combined transfer starts with START, places repeated START between
messages, and finishes with STOP. A message has an address, direction, and byte
buffer. A register address is device-specific data in that buffer, not a separate
I²C field. Separate write and read calls introduce a STOP and allow another
transfer between them. A device may treat that differently from a combined
transfer. [Linux I²C protocol](https://www.kernel.org/doc/html/latest/i2c/i2c-protocol.html)

`i2ctransfer` exposes an ordered message list and can change addresses between
messages. Its core model is appropriate for the proposed backend. Recent
upstream versions also document optional message flags; v1 of this service
deliberately omits those flags, variable-length receives, and 10-bit addressing.
[Upstream i2ctransfer manual](https://kernel.googlesource.com/pub/scm/utils/i2c-tools/i2c-tools/+/master/tools/i2ctransfer.8)

Example: `i2ctransfer -y 1 w2@0x50 0x12 0x34 r4` writes the two bytes `12 34`
and reads four bytes at address `0x50` in the same transfer. Calling that a
register read is justified only by the particular device's datasheet.

## i2cget and i2cset are not generic register APIs

`i2cget` without a data address uses SMBus Receive Byte. Its default with a
data address is SMBus Read Byte Data. Mode `w` reads a word; `c` performs a
Send Byte followed by Receive Byte, as separate transactions. Upstream also
documents block modes `s` and `i`. These choices must not be collapsed into an
unspecified service `read` operation.
[Upstream i2cget manual](https://kernel.googlesource.com/pub/scm/utils/i2c-tools/i2c-tools/+/master/tools/i2cget.8)

`i2cset` supports short writes without a value, byte/word data, and two kinds
of block writes. A short write can change a pointer or actually write device
state. Masked writes and readback assume device-specific read/write symmetry;
they are not general atomic register operations.
[Upstream i2cset manual](https://kernel.googlesource.com/pub/scm/utils/i2c-tools/i2c-tools/+/master/tools/i2cset.8)

SMBus adds specific command framing. Word data is transmitted low byte first;
SMBus block transfers carry a count byte and traditionally support up to 32
payload bytes through these Linux interfaces. I²C block operations do not use
the same count framing. PEC is a separate SMBus error-checking facility. A raw
I²C read of two bytes neither interprets a word nor automatically adds PEC.
[Linux SMBus protocol](https://www.kernel.org/doc/html/latest/i2c/smbus-protocol.html)

| Tool operation | Proposed v1 equivalent | Qualification |
| --- | --- | --- |
| `i2ctransfer -y 1 r4@0x50` | `read`, address 80, length 4 | Raw read. |
| `i2ctransfer -y 1 w2@0x50 0x10 0xab` | `write`, data `[16,171]` | One raw write message. |
| `i2ctransfer -y 1 w1@0x50 0x10 r4` | `fetch`, data `[16]`, length 4; or `read` with register `[16]` | Repeated START between messages. |
| `i2cget -y 1 0x50 0x10 b` | `read` with register `[16]`, length 1 | Comparable wire framing without PEC on an I²C-capable adapter; not an SMBus-only fallback. |
| `i2cget -y 1 0x50 0x10 w` | `read` with register `[16]`, length 2 | Result is two bytes; client interprets low-byte-first if using SMBus semantics. |
| `i2cget ... c` | Separate `write` and `read` | Not equivalent to `fetch`; no intervening STOP in fetch. |
| `i2cset ... b` / `w` | `write` with explicit command and data bytes | Client supplies all byte ordering; no automatic readback. |
| SMBus block, PEC, process call | Explicit SMBus extension, deferred | No silent emulation or claim of full tool compatibility. |

## Enumeration, functionality, and scanning

`i2cdetect -l` lists installed adapters; `-F` reads their functionality. Scanning
is different: there is no standard universally harmless discovery command, so
the tool sends SMBus probes. A failed probe is not proof that an address is
unused. The service should expose configured adapters only and label scan
results as probe observations.
[Upstream i2cdetect manual](https://kernel.googlesource.com/pub/scm/utils/i2c-tools/i2c-tools/+/master/tools/i2cdetect.8)

The upstream automatic scan selects Receive Byte for `0x30–0x37` and
`0x50–0x5f`, and Quick Write for other regular addresses. It skips probes whose
capabilities are missing, and checks each address for a bound kernel driver.
The proposed `auto` scan follows that selection, but preserves detailed outcomes
instead of merging all errors into an absent-device display.
[Upstream scan implementation](https://kernel.googlesource.com/pub/scm/utils/i2c-tools/i2c-tools/+/master/tools/i2cdetect.c)

## Linux interface and limitations

The device interface offers `I2C_FUNCS` for capability inspection, `I2C_RDWR`
for combined transfers, and `I2C_SMBUS` for SMBus operations. `I2C_RDWR` needs
`I2C_FUNC_I2C`; every message supplies its address. Adapter numbering can vary,
so the service uses an explicit configured path, not a parsed bus name.
[Linux userspace interface](https://www.kernel.org/doc/html/latest/i2c/dev-interface.html)

Capabilities describe adapter operations, not the identity or register layout
of attached devices. Check the relevant operation flag rather than assuming
all adapters implement all I²C/SMBus functions.
[Linux functionality reference](https://www.kernel.org/doc/html/latest/i2c/functionality.html)

The UAPI defines a maximum of 42 messages per `I2C_RDWR` request. Its structures
contain native pointers and an unsigned-long capability output, which matters
for ABI correctness across architectures.
[i2c-dev UAPI](https://raw.githubusercontent.com/torvalds/linux/master/include/uapi/linux/i2c-dev.h)
The current driver additionally limits each message to 8192 bytes. Adapter
quirks can impose smaller limits. Successful `I2C_RDWR` returns a message count;
a smaller count must not be reported as complete success.
[Linux i2c-dev implementation](https://raw.githubusercontent.com/torvalds/linux/master/drivers/i2c/i2c-dev.c)

There is an ownership trap: raw transfers bypass `I2C_SLAVE`'s driver-busy
check. `i2ctransfer` explicitly calls the address-selection helper before
submitting messages unless forced. The service must check every distinct
message address too. This is a best-effort check, not a lasting kernel lease.
[Upstream transfer implementation](https://kernel.googlesource.com/pub/scm/utils/i2c-tools/i2c-tools/+/master/tools/i2ctransfer.c)

Bus errors need context. `ENXIO` can mean no ACK during addressing; `EAGAIN`
can indicate arbitration loss; `ETIMEDOUT` indicates a timeout. Do not promise
that every driver distinguishes address NACK, data NACK, and general I/O
failure. Preserve the underlying errno and operation stage.
[Linux fault codes](https://www.kernel.org/doc/html/latest/i2c/fault-codes.html)

An asynchronous Rust wrapper does not make an ioctl cancellable. Once a Tokio
blocking task starts, abort does not stop it. The proposal retains resource
leases until actual completion and has no promise of a hard transfer deadline.
[Tokio spawn_blocking contract](https://docs.rs/tokio/latest/tokio/task/fn.spawn_blocking.html)

## Deployment implications

The SDA/SCL numbers in this repository's ideas agree with its Rock5B data:
header resource 8 maps to GPIO0_B5, line 13, and I2C1_SCL_M0; resource 10 maps
to GPIO0_B6, line 14, and I2C1_SDA_M0. See
[board data](../assets/rock5b/gpio.json) and
[sample configuration](../assets/rock5b/config.toml).
These entries do not prove that the running OS has enabled that route or
assigned it `/dev/i2c-1`. The board configuration and running system must agree.
The service should neither request SDA/SCL as GPIO nor switch pinmux itself.
