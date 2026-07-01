# Ferrix Compatibility Notes

Ferrix does not promise a stable public 1.0 API yet, but compatibility-sensitive
surfaces must be changed intentionally and tested.

## Tracked Versions

- Ferrix CLI version: workspace package version.
- Ferrix runtime daemon version: `ferrix-runtime` package version.
- Runtime protocol version: `CURRENT_PROTOCOL_VERSION`.
- Runtime protocol supported range: `MIN_SUPPORTED_PROTOCOL_VERSION` to
  `MAX_SUPPORTED_PROTOCOL_VERSION`.
- Bytecode container version: `BytecodeFormat.version`.
- Source language version: not introduced yet.

## Compatibility Rules

- Runtime daemon protocol changes must update protocol feature names or version
  constants when clients need to behave differently.
- Bytecode container changes that break old readers must bump the container or
  bytecode format version.
- CLI human output may evolve, but golden-covered command output should only
  drift with a test update and an explanation.
- CLI JSON output is intentionally small. New fields may be added; changing or
  removing existing fields requires a migration note.
- Runtime error categories are stable identifiers. Renaming a category requires
  a migration note.

## Migration Notes

### 0.1.0

- Runtime protocol `1.0` introduced with lifecycle, process history, bytecode
  container, request identity, and basic middleware features.
- Runtime daemon inspection commands introduced for status, metrics, events,
  and config.
- CLI JSON output introduced for selected inspection commands.
- Bytecode container metadata inspection is available through `ferrix inspect`.
