# ADR-001: Unify Channel API Under SourceInstanceId

**Status**: Accepted

## Context

The audio engine has two channel creation paths that produce the same
`ChannelStrip` internally:

| Path | Addressing | Effects via RT commands |
|------|-----------|----------------------|
| Legacy `add_channel(SourceId, source)` | Fixed enum (`System/Mic/Player/Glicol`) | gain, pan, HPF, LPF, enable |
| Instance `add_file_source_instance(req)` | Free-form `SourceInstanceId` string | gain, pan, HPF, LPF, **EQ, reverb, rate, reverse** |

This means Glicol, Mic, TestTone, and MusicalTest channels created via
the legacy path cannot receive EQ, reverb, or other effects through the
RT command queue, even though the underlying `ChannelStrip` fully
supports them.

The dual API also forces every consumer to know which path was used and
pick the matching command variant (`SetChannel*` vs `SetSourceInstance*`).

## Decision

Remove the legacy `SourceId`-addressed channel API and unify all channel
creation under `SourceInstanceId`.

### Specifics

1. **Add `add_source_instance(id, kind, asset_ref, timeline, source)`** as
   the single channel creation entry point. `add_file_source_instance`
   stays as a convenience wrapper that decodes bytes then delegates.

2. **Add `SetSourceInstanceEnabled` RT command** to replace
   `SetChannelEnabled`. This is distinct from `stop_source_instance`
   which fades and marks transport as Stopped.

3. **Remove 5 legacy RT command variants**: `SetChannelGainDb`,
   `SetChannelPan`, `SetChannelHighpassHz`, `SetChannelLowpassHz`,
   `SetChannelEnabled`.

4. **Remove legacy infrastructure**: `ChannelStrip::new` (legacy
   constructor), `legacy_source_id` field, `LegacySourceBridge`,
   `PlaybackStatusAuthority::LegacyChannelEnabled`,
   `SourceTimelinePlacement::legacy_always_on()` (renamed to
   `always_on()`), `SourceKind::from_legacy_source`.

5. **Remove `SourceId` enum** from `omm-protocol`. Channel addressing
   uses `SourceInstanceId`, type classification uses `SourceKind`.

6. **Migrate `FeatureAnalyzerHandle`** from `SourceId`-keyed to
   `SourceInstanceId`-keyed.

7. **Migrate `omm-protocol` IPC messages** (`RtTarget::Source`,
   `SetSourceMute`) from `SourceId` to instance addressing.

### Commit Strategy

Three commits to keep bisect-friendly:

1. Add generic `add_source_instance` + `SetSourceInstanceEnabled` alongside
   legacy (both paths compile).
2. Migrate all call sites (main.rs demos, tests) to instance API.
3. Remove legacy code paths and `SourceId` enum.

## Consequences

- All source types get uniform access to the full effects chain via RT
  commands.
- No more dual addressing; one path for IPC, CLI, and tests.
- `SourceInstanceId` strings are caller-chosen, so naming conventions
  (e.g. `"glicol:main"`, `"mic:default"`) replace the fixed enum.
- ~100+ call site changes across production and test code.
- `SourceId` removal is a breaking change for `omm-protocol` message
  schemas. This is acceptable because IPC is not yet wired.
