# Proposed I²C backend interfaces

Status: review draft. Type sketches are contracts to discuss, not implemented
Rust. [Architecture](i2c-architecture.md) owns session leases and worker policy;
[protocol](i2c-protocol.md) owns JSON and byte encodings.

## Common contract

The backend deals only in bytes and operations. It knows nothing about protocol
request IDs, named targets, GPIO leases, `register`, hex, or base64. Addresses
remain part of each call. Blocking mutable methods make descriptor state and
serialization explicit; the manager exposes asynchronous completion to sessions.

```rust
pub trait I2cBackend: Send + Sync {
    fn open_adapter(&self, device: &str)
        -> Result<Box<dyn I2cAdapter>, I2cError>;
}

pub trait I2cAdapter: Send {
    fn identity(&self) -> &AdapterIdentity;
    fn capabilities(&mut self) -> Result<AdapterCapabilities, I2cError>;
    fn transfer(&mut self, plan: &TransferPlan)
        -> Result<TransferResult, I2cError>;
    fn probe(&mut self, address: Address7, method: ProbeMethod)
        -> Result<ProbeObservation, I2cError>;
}

pub enum Message {
    Write { address: Address7, data: Box<[u8]> },
    Read { address: Address7, length: usize },
}

pub struct TransferPlan {
    pub messages: Vec<Message>,
}

pub struct ReadData {
    pub message_index: usize,
    pub data: Box<[u8]>,
}

pub struct TransferResult {
    pub completed_messages: usize,
    pub reads: Vec<ReadData>,
}

pub enum ProbeMethod { QuickWrite, ReceiveByte }

pub struct I2cError {
    pub kind: I2cErrorKind,
    pub stage: I2cStage,
    pub os_errno: Option<i32>,
    pub completed_messages: Option<usize>,
    pub effect: Effect, // None or Possible
    pub context: String,
}
```

`Address7` is constructed only through range validation. `AdapterIdentity`
distinguishes Linux device numbers from mock canonical paths.
`AdapterCapabilities` keeps the raw functionality mask and typed predicates
such as `supports_i2c`, `supports_quick_write`, and `supports_receive_byte`.
It does not claim that every legal transfer shape is supported by an adapter.
`I2cStage` distinguishes validation, open, capabilities, address check, transfer,
and probe; `I2cErrorKind` covers unsupported, busy, NACK, timeout, partial, I/O,
and internal errors. The session adds its bus name when constructing a reply.

Backend invariants:

1. Validate the whole plan and its sizes before sending any message, including
   when called outside the JSON path. Then check capabilities and every unique
   address for kernel/model ownership. A later busy address prevents all traffic.
2. `transfer` submits the entire ordered list as one operation. Never implement
   it as a loop of separate read/write calls; never split on addresses or sizes.
3. Return `Ok` only when all messages and expected read bytes completed.
   Positive short results are errors carrying the completed message count;
   negative errors may have unknown progress. Do not return unread buffer data.
4. Do not automatically repeat a transfer or probe. Kernel/controller retries
   may still occur according to system policy; this is not an exactly-once bus.
5. `probe` returns ACK/no-ACK/busy/unsupported/ambiguous error observations as
   appropriate. Fatal adapter removal and internal failures propagate as errors.
   Scan loops live above the backend, ensuring both backends share policy.
6. No method reserves GPIO lines or changes pinmux, frequency, or ownership
   policy. `open_adapter` and `capabilities` generate no address traffic.

## Linux implementation

Use a small Linux-only UAPI module called by `SysI2cBackend` and `SysI2cAdapter`.
The existing `libgpiod` abstraction cannot provide I²C. The recommendation is a
direct ioctl wrapper using the existing `nix` ecosystem or `libc` explicitly,
with all unsafe layout/pointer work contained in one file. Choose the exact
dependency in implementation review; no Cargo change is part of this draft.

An alternative is the Rust `i2cdev` crate's adapter-level `LinuxI2CBus` transfer
API. Audit its access to capability bits, address-busy checking, probe methods,
and short-result reporting before substituting it. A fixed-address device object
alone does not model this draft's dynamic, multi-address transfer contract.
[LinuxI2CBus documentation](https://docs.rs/i2cdev/latest/i2cdev/linux/struct.LinuxI2CBus.html)

| Contract step | System call / operation |
| --- | --- |
| Open | Open configured path with read/write access and close-on-exec; own the `File`/`OwnedFd`. Require a character device and successful capability ioctl. |
| Identity | `fstat` and character-device identity, then deduplicate aliases in the manager. |
| Capabilities | `ioctl(I2C_FUNCS, &mut c_ulong)`. |
| Address check | `ioctl(I2C_SLAVE, address)` for every distinct address before transfer, never `I2C_SLAVE_FORCE`. |
| Transfer | One `ioctl(I2C_RDWR, &mut i2c_rdwr_ioctl_data)` containing all `i2c_msg` entries. |
| Quick probe | Address check, then `I2C_SMBUS` with WRITE + QUICK; no payload. |
| Receive-byte probe | Address check, then `I2C_SMBUS` with READ + BYTE; discard received byte after recording observation. |
| Close | Drop the owned descriptor after pending work has completed. |

These operations follow the [Linux userspace interface](https://www.kernel.org/doc/html/latest/i2c/dev-interface.html)
and the [i2c-dev UAPI](https://raw.githubusercontent.com/torvalds/linux/master/include/uapi/linux/i2c-dev.h).
Use exact `#[repr(C)]` structs, C-width integers, and native pointers. In
particular `I2C_FUNCS` uses `c_ulong`, message lengths are `u16`, and `nmsgs` is
`u32`. The relevant ioctls are legacy request numbers; do not accidentally
generate different values with size/direction-encoded ioctl macros.

Allocate all read/write buffers before creating native message pointers. Read
buffers start initialized, and neither their allocations nor the message vector
can move/reallocate during the call. Use only `I2C_M_RD` for read messages in v1;
all other flags are zero. Preserve consecutive messages without coalescing.
See the [message UAPI](https://raw.githubusercontent.com/torvalds/linux/master/include/uapi/linux/i2c.h).

Because message addresses override descriptor selection, the sequence of busy
checks can finish on any address; it does not change the submitted addresses.
The check is best-effort: another driver may bind afterward. Opening the adapter
also does not reserve devices against another userspace process. Worker
serialization protects this service's descriptor mutations, and session leases
protect only this service's clients.

Interpret `I2C_RDWR` success as a message count, not zero or a byte count. An
unusual positive count exceeding the requested count is an internal error.
Preserve all errno values; classify `EBUSY` during address selection as
`device_busy`, whereas `EBUSY` during transfer is a bus I/O failure. Classify
`ENXIO` as no ACK, `ETIMEDOUT` as timeout, `EOPNOTSUPP` as unsupported, and
other failures as I/O unless the driver supplies more information. Negative
transfer results always have uncertain effects. Do not retry `EINTR`/`EAGAIN`
at this layer without a future explicit operation-specific policy.

Build only the system implementation on Linux, following GPIO's target gating.
No command execution, runtime `i2c-tools` dependency, or new `libi2c` dependency
is required for the direct wrapper. Do not alter adapter-wide `I2C_TIMEOUT` or
`I2C_RETRIES` to implement client deadlines.

## Mock backend

Use a process-local adapter registry shared by backend clones, analogous to
the current GPIO mock's shared chip registry. Open handles for one canonical
fixture share device state, driver-busy state, and transaction trace. Separate
backend instances are isolated. The fixture is a seed loaded once, not a live
state file; writes persist in memory until that backend is dropped. Restart
reloads the seed. No polling watcher or implicit disk rewrite is introduced.

Suggested fixture (new TOML format, unrelated to GPIO XML):

```toml
version = 1
name = "mock adapter 1"
capabilities = ["i2c", "smbus_quick", "smbus_read_byte"]

[[devices]]
address = 0x50
model = "register_memory"
size = 256
pointer_bytes = 1
pointer_endian = "big"
auto_increment = true
wrap = true
reset_pointer_on_stop = false
read_only = false
fill = 0
initial = [{ offset = 16, data = [17, 34, 51, 68] }]
probe_quick = "ack"
probe_receive = "read"

[[devices]]
address = 0x52
model = "register_memory"
size = 256
pointer_bytes = 1
pointer_endian = "big"
auto_increment = true
wrap = true
reset_pointer_on_stop = false
read_only = false
fill = 0
initial = []
driver_busy = true
probe_quick = "ack"
probe_receive = "read"
```

Require all model fields shown except `driver_busy` (default false). Reject
unknown keys, duplicate addresses, invalid IDs/bytes/initial ranges, overlapping
initial ranges, invalid capability names, and zero or excessive sizes at load.
V1 supports 1- or 2-byte pointers, big or little endian, and size at most
`min(65536, 256^pointer_bytes)`. Limit each fixture to 112 regular-address
devices and 8 MiB total seed memory. Fixture capability names use the same
individual flags as the query schema; mock execution must enforce them.

The register-memory model is an explicit test device, not a claim about all
I²C devices or any particular EEPROM. It behaves as follows:

- Each write message starts with exactly `pointer_bytes` bytes encoding the
  new pointer. A shorter message fails with a model I/O error. Exactly that
  many bytes sets only the pointer; trailing bytes write memory sequentially.
- Reads return bytes starting at the pointer. When `auto_increment=true`,
  advance after each data byte for both reads and writes. Otherwise repeatedly
  access the selected cell. `wrap=true` wraps increment at `size`; false leaves
  an exhausted pointer and fails when a subsequent byte would be out of range.
  An initially encoded pointer outside the memory range is an error, not modulo.
- A read-only model accepts pointer-only writes and rejects writes with data.
  A new pointer may already have been consumed before a later write failure;
  traces and errors must not claim rollback.
- Repeated START preserves pointer state. At transfer termination, apply
  `reset_pointer_on_stop` to all addressed models. This allows a test where
  fetch works but separate write/read does not. On injected transfer failure,
  model a terminating STOP for cleanup; document that real recovery can differ.
- Quick probes with `probe_quick="ack"` return ACK without changing memory.
  `probe_receive="read"` performs a one-byte read (including pointer movement)
  and discards the byte, followed by STOP behavior. Either probe setting can
  instead be `"no_ack"`. Capability rejection precedes device dispatch.
- Absent addresses return no ACK. `driver_busy=true` prevents traffic during
  address checks. A device can have a probe fail yet still accept normal
  transfers, demonstrating why scans are not inventories.

The adapter dispatches messages in order and preserves their boundaries. For
mixed-address transfers, validate ownership for every address before executing,
but do not preflight device responses: a later absent address must be able to
fail after earlier writes. All messages of a transfer hold the shared mock
adapter-state lock, preventing interleaving by another handle.

For non-register protocols, add a `ScriptedDevice` test helper alongside the
fixture model. A script expects direction, address, bytes/length, and transfer
boundary and supplies read bytes or an error. Unmatched operations fail
explicitly. This validates sensors with commands/FIFOs without teaching the
generic backend fake universal register semantics. Persisted script syntax can
be added later if useful; it is not part of this v1 fixture grammar.

Suggested test control interface, excluded from the socket protocol:

```rust
impl MockI2cBackend {
    fn snapshot(&self, adapter: &AdapterIdentity) -> MockAdapterSnapshot;
    fn trace(&self, adapter: &AdapterIdentity) -> Vec<MockTraceEntry>;
    fn set_driver_busy(&self, adapter: &AdapterIdentity, address: Address7, busy: bool);
    fn inject_next(&self, adapter: &AdapterIdentity, fault: MockFault);
}
```

Trace entries record transfer/probe kind, exact ordered message boundaries,
input/output bytes, completion count, failure stage, and a sequence number.
Metadata queries create no traffic entries. Use a bounded trace (e.g. the most
recent 1024 entries) so long-running mocks do not grow forever. Faults include
pre-submission failure, address NACK, positive partial completion after message
N, ambiguous transfer error after effects, and a controlled completion gate.
Use that gate in concurrency tests instead of flaky wall-clock sleeps.

Snapshots are read-only copies for assertions. No test control action or memory
dump is exposed to normal clients. If manual mock inspection needs a persistent
log later, add an independent optional JSONL trace rather than modifying GPIO's
existing XML write-log format.

## Verification plan for the backend boundary

| Test area | Evidence required |
| --- | --- |
| Transfer compilation | Each convenience verb creates the exact expected messages; endian/prefix bytes are preserved. |
| Codecs and limits | Encodings round-trip arbitrary bytes, including zero and 255; malformed/oversized input produces no traffic. |
| Repeated START | A reset-on-STOP model distinguishes fetch from separate writes/reads. |
| Partial failure | A later failing address leaves earlier effects visible and never returns fabricated read success. |
| Capability checks | Raw-disabled and probe-disabled mocks match system unsupported behavior. |
| Ownership | Aliases share state/leases, busy addresses block the whole plan, and GPIO/SDA/SCL conflicts are rejected. |
| Lifecycle | A gated operation retains the lease after disconnect; queued work cancels; late completion cannot reply to a closed session. |
| Reactor responsiveness | GPIO events and scheduled steps continue while a fake adapter is gated. |
| Linux syscall fake | Exact ioctl arguments, flags, native buffer contents, address checks before transfer, short counts, errno preservation, and descriptor cleanup. |
| ABI and hardware | Check struct sizes/offsets against target UAPI on x86_64 and aarch64; verify a known peripheral on Rock5B when hardware is available. |

Mock/device tests and syscall-fake tests prove different things. Also consider
Linux `i2c-stub` for later SMBus-specific checks, but do not treat it as proof
that a controller supports arbitrary combined raw I²C transfers.

## Explicit future SMBus extension

D2 accepts raw I²C first; this extension is deferred beyond the initial scope.
The proposed internal scan probes remain conditional on D4. This extension
covers Linux userspace transactions, not ARP, Alert, or Host Notify.

If this later extension is approved, add a separate typed `SmbusOperation` family
and `I2cAdapter::smbus(address, operation, pec)` method. Operations distinguish
receive/send byte, read/write byte data, read/write word data, SMBus block,
fixed-length I²C block, and process calls. Each checks its exact functionality
bit; command width, word byte order, counts, and PEC are explicit.

Expose it with an explicit transaction discriminator or separate `smbus` action,
to be reviewed before implementation. Do not overload raw `register` or infer
SMBus from buffer size. Set/reset descriptor PEC state for each SMBus call under
worker ownership, so a previous request cannot change later semantics. Mock
SMBus support must model count/PEC behavior and error paths separately; a
register-memory raw transfer alone is insufficient evidence of compatibility.
