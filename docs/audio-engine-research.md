# RAGE audio injection research

## FiveM reference

FiveM's native voice route is a useful design reference, not a GTA IV drop-in:

- `code/components/extra-natives-five/src/NuiAudioSink.cpp` creates a persistent RAGE sound
  (`audExternalStreamSound`), initializes its stream player against an
  `audReferencedRingBuffer`, and starts it through `PrepareAndPlay`.
- It marks the ring buffer with a custom ID and hooks the stream player's frame generation.
  On each mixer request it finds the associated audio entity and asks it to produce the needed
  samples. The callback then continues through RAGE's original frame generator.
- The sound is configured as positional and assigned an audio environment group. Position and
  interior information are updated on the game side; RAGE owns final mixing and spatial playback.
- `code/components/voip-mumble/src/MumbleAudioOutput.cpp` exposes an `IMumbleAudioSink` route.
  Incoming voice is decoded to PCM and delivered as `PushAudio` blocks; when native audio is
  enabled, the sink forwards those blocks to the RAGE audio entity. The default voice route also
  has a separate XAudio2 path.

This is materially different from replacing a wave-bank sample: it injects a PCM stream into a
RAGE sound object and uses the normal audio mixer. It provides a good shared backend for resource
sounds and future voice, provided GTA IV has a compatible stream-sound path.

## GTA IV findings

- GTA IV's user-track path at `0x883310` selects a decoder by extension. Its worker at `0x8834A0`
  feeds decoded sample buffers through `CWMStreamReader`; this is a radio stream, not positional
  audio.
- The GTA IV dump contains `audStreamingSound` and `audSimpleSound` type names, but no
  `audExternalStreamSound` or `audReferencedRingBuffer` type-name strings. Therefore FiveM's class
  layout, signatures, and pattern scans must not be copied into the 32-bit GTA IV DLL.
- GTA IV does have a `WaveSlot` / `WaveSlotsConfig` path. `FUN_008870e0` reads `WaveSlotsConfig`,
  `RpfConfig`, `RpfDirectory`, `WaveRootPath`, and `EffectsRootPath`; this is configuration/asset
  setup, not yet evidence of a runtime PCM stream-player equivalent.
- `FUN_00888310` parses `audio:/config/waveslots.xml`. For entries whose `LoadType` is `STREAM` it
  builds a stream-slot object through `FUN_008923d0`; other entries use `FUN_00887b00`. The stream
  slot's file path is resolved through the audio RPF/ZIP path (`FUN_00888df0`), so this is a
  file-backed bank/slot stream, not yet the FiveM-style producer-fed ring buffer.
- The vehicle engine stream-slot table is initialized by `FUN_0098ce40`: it looks up
  `STREAM_ENGINE_1` through `STREAM_ENGINE_25` and records the available slot IDs. The lookup is
  `FUN_00887df0`; `FUN_0098ecb0` allocates/claims a slot. These are vehicle audio slots and should
  not be mistaken for arbitrary custom positional PCM voices.
- A separate GTA IV Complete Edition research project (`gutbash/revd`) found the vehicle engine
  sound slot table and reports that the game resolves named `STREAM_ENGINE_n` slots from
  `waveslots.xml`. That confirms the slot-based asset path is real, but does not show how to feed
  arbitrary runtime PCM into a positional sound.
- The existing IVMP script sounds still use the CEF/WebAudio route. That is not the RAGE backend.

### Follow-up trace (2026-10-26)

- The handoff mentioned in the conversation is `.claude/handoff.md` (there is no
  `docs/handoff.md` in this checkout). Its instructions also confirm that Ghidra is
  available through the HTTP API and that Ghidra findings should be annotated there.
- Confirmed the `STREAM` slot update loop: `FUN_008893a0` calls `FUN_00893cb0` for
  stream slots. `FUN_00892df0` performs a two-buffer update under engine synchronization.
  The nonzero `+0x98` branch calls `FUN_00883420` to produce samples, while the zero branch
  reads archive-backed data. This is still a wave-slot update path, not proof of an
  independent positional stream sound.
- `AudioStreamWaveSlot_Init` sets `+0x94` and `+0x98` to zero. The decoder-backed branch
  expects `+0x94` to be a vtable-bearing decoder/source object and uses its methods at
  offsets `+0x10`, `+0x14`, and `+0x18`; `+0x98` is supplied as the decoder-open source
  argument. The initialization and update routines alone do not show who can install this
  pair or its ownership rules.
- The executable contains RTTI/type-name strings for `audSound`, `audStreamingSound`,
  `audSimpleSound`, and multiple game audio entities (including pedestrian, vehicle,
  weapon, emitter, explosion, and frontend entities). The presence of these names is
  evidence of the class hierarchy, not evidence that GTA IV has FiveM's
  `audExternalStreamSound` or a caller-fed ring buffer. Several names are in RTTI data and
  have no direct code xrefs in Ghidra.
- The next decisive trace is the GTA IV `audStreamingSound` constructor/vtable and the
  sound-player/entity registration path that instantiates it. Separately trace script
  positional-sound natives into the same sound-player layer. Only if a `STREAM` slot
  reaches that positional mixer path should the slot route be considered as a candidate.
- The checked-in native metadata identifies `PLAY_SOUND_FROM_POSITION` (`0x65752C65`)
  as taking an integer, a string, and three floats (`isfff`). This is a useful positional
  playback entry point to trace, but its string-based interface does not itself expose a
  PCM buffer or arbitrary stream source. It likely starts a game-defined sound by name;
  confirm that by following its native handler rather than treating the signature alone
  as conclusive.
- Traced that native in Ghidra: its registration is in `FUN_009CDF90`, which binds hash
  `0x65752C65` to `0x009CC130` (`Native_PlaySoundFromPosition`). The handler forwards
  parameters into `0x009CD3E0` → `0x009A6D90` → `0x009A6B80`
  (`ScriptAudio_PlayNamedSound`). That path records/resolves the script audio name, then
  calls `0x00E39230` → `0x00E39780` → `0x008A9A50` → `0x008A9A90`
  (`AudioSound_CreateFromDefinition`).
- `AudioSound_CreateFromDefinition` resolves an audio sound definition (`FUN_00894880`
  selects the appropriate manager, then `FUN_008948C0` looks up an audio object and invokes
  its virtual method at `+0x1c` to configure/start it). This strongly supports that the
  script positional native plays a **bank-defined named sound** through RAGE's sound
  manager, not an arbitrary file or PCM buffer. The resulting object is a real game sound
  object, so this is the right positional/mixer chain to study for native occlusion
  behavior, but the factory still offers no direct PCM-source seam.
- Added Ghidra names and `IVMP:` comments for the native handler, named script playback
  manager, and definition-based sound factory. Next trace the manager's object lookup,
  the sound object's vtable implementations (especially `audStreamingSound`), and the
  mixer-side frame request. Goal: determine whether a RAGE sound definition can be
  instantiated with a replaceable/producer-backed stream without replacing spatialization.

### `audStreamingSound` trace (confirmed)

- `FUN_00894A20` is the sound-class factory used by the named-sound path. Factory type `8`
  allocates `0xE8` bytes and calls `0x0089EB80`, now named `audStreamingSound_ctor`.
  This conclusively identifies GTA IV's `audStreamingSound` constructor and its vtable
  (`0x00E7A454`).
- The factory's common virtual configuration dispatch is at vtable offset `+0x1C`.
  For an `audStreamingSound` object that target is `0x0089F560`
  (`audStreamingSound_Configure`). It initializes base sound data, takes stream-definition
  metadata from the definition object, then calls `0x0089F2C0`
  (`audStreamingSound_CreateTracks`).
- `audStreamingSound_CreateTracks` accepts at most eight track definitions. For each it calls
  `FUN_008947D0`, which resolves a named audio sound and routes it back through the normal
  audio-object factory. The created child sound handles are retained by the streaming object.
  Its destructor releases these child sounds and their referenced audio resources.
- This is an important correction: in this build, `audStreamingSound` is demonstrably a
  **RAGE sound-definition/track container**, not yet a directly exposed caller-writable PCM
  ring buffer. It is still a valid native positional sound object once created through the
  entity/sound manager, but its currently traced input is named game audio tracks.
- Ghidra annotations added: `audStreamingSound_ctor`, `audStreamingSound_dtor`,
  `audStreamingSound_Configure`, and `audStreamingSound_CreateTracks`. The next practical
  trace is the child track type(s) produced by a streaming definition, especially the
  wave-slot-backed type, and then its mixer read callback / wave-buffer ownership.

### Positional wave-slot consumer and runtime file source

- Factory type `12` is `audSimpleSound` (`audSimpleSound_ctor` at `0x008A29A0`).
  Its configuration virtual is `audSimpleSound_Configure` (`0x008A2D70`). That method
  resolves a wave-slot and clip entry from the named simple-sound definition, then records
  the wave metadata used for playback.
- For a `STREAM` slot, the simple-sound path calls `FUN_008895E0`, which dispatches to
  `FUN_00893DA0`; this reads the active streamed wave metadata. Therefore an
  `audSimpleSound` is the presently traced game sound object that consumes a `STREAM`
  wave-slot. When created via the positional script/entity sound path, it is the relevant
  RAGE 3D/occlusion-side consumer.
- Found the missing writer for stream-slot `+0x94` / `+0x98`:
  `AudioStreamWaveSlot_SetSourceFile` at `0x008928A0`. It accepts a wide file path, creates
  the decoder wrapper at `slot + 0x94`, and owns a copied source path at `slot + 0x98`.
  `AudioStreamWaveSlot_ClearSourceFile` (`0x00892A00`) tears both down.
- Once that source is present, `FUN_00893CB0` opens it via
  `UserTrackDecoder_OpenByExtension` and fills the stream slot's PCM buffers. This is the
  first confirmed GTA IV runtime **file-to-PCM-to-RAGE STREAM slot** path.
- This does **not** yet deliver an implementation: the sole in-game caller is currently
  `0x00B769A0`, and the remaining proof is to identify a safely claimable STREAM slot plus
  a positional named `audSimpleSound` definition that references it. The decoder path's
  accepted formats and thread/lifetime behavior must also be verified in game before
  exposing `LoadAudioFile`.

### Strong candidate: GTA IV's positioned radio emitter

- The one caller of `AudioStreamWaveSlot_SetSourceFile` is an in-game component now labelled
  `PositionedRadioEmitter_StartStream` (`0x00B769A0`). Its update state machine creates a
  sound named `POSITIONED_RADIO_EMITTER`; the code explicitly verifies that its created sound
  has type `8`, the now-confirmed `audStreamingSound` type.
- `PositionedRadioEmitter_SetFileSource` (`0x00B767F0`) stores a caller-supplied source path,
  enables dynamic-stream mode, and the later start routine sends that path into the claimed
  `STREAM` wave slot using `AudioStreamWaveSlot_SetSourceFile`.
- The component creates the `POSITIONED_RADIO_EMITTER` named RAGE sound and prepares its
  streaming-object track voices, then clears the stream source on teardown. The name is
  promising, but it is **not proof** that a mod-created instance receives world coordinates,
  wall occlusion, or an audio environment. Those properties must be traced from an actual
  position setter into the audio entity/mixer before it is proposed as the implementation.
- Therefore this is only a runtime-file decoder/stream-slot lead, not the solution claimed for
  `LoadAudioFile`. The correct next task is to trace the genuine positional audio-entity
  creation and position update calls, then establish whether this template can be bound to that
  entity without using the non-positional user-track radio player.

### Confirmed world-position handoff

- The `PLAY_SOUND_FROM_POSITION` native's x/y/z arguments were traced all the way into the
  created RAGE sound instance. `AudioSound_CreateWithWorldPosition` (`0x00E39780`) creates and
  configures the named audio object, then writes those three floats to object offsets
  `+0xA4`, `+0xA8`, and `+0xAC`.
- This confirms that the positional native is not merely a script-level label: world-position
  data reaches the live audio object. It is the appropriate creation route for a future custom
  source *after* a stream-backed sound definition/slot is safely bound to it.
- This still does not establish the exact occlusion/environment calculation. The next trace is
  the mixer/voice update that consumes those sound-instance fields and applies room, interior,
  and obstruction state.
- No audio hook or `LoadAudioFile` implementation has been added. Do not infer that the
  user-track decoder provides RAGE occlusion merely because it produces PCM.

## Port plan

1. In GTAIV.exe, identify the sound-entity factory, persistent sound creation path, stream-player
   initializer, ring-buffer read/write routines, and mixer frame callback. Confirm object layouts,
   calling conventions, buffer ownership, supported PCM format, and teardown behavior in Ghidra.
2. Prove a minimal mono PCM stream through a game-owned positional sound and confirm the expected
   distance/interior/occlusion behavior before adding file decoding.
3. Add one bounded PCM producer interface used by both file playback and voice playback. Decode
   MP3/WAV off the mixer thread; the mixer callback must only copy available PCM or silence and must
   never block, allocate, or decode.
4. Make `LoadAudioFile(path)` create/cache a resource-scoped decoded source. A later play call should
   create a RAGE stream voice from that source; an eventual voice receiver can feed the same producer
   with decoded network PCM.
5. Keep CEF playback as a fallback until the IV mixer path is proven in game.

## Safety constraints

- Use GTA IV addresses and layouts only; FiveM's current GTA V code is 64-bit and build-specific.
- Keep game object creation/destruction on the appropriate game thread and PCM writes bounded and
  synchronized with the audio callback.
- Do not hold script, network, or resource locks while the mixer requests samples.
- Test cleanup during resource stop, session reload, and DLL shutdown to avoid dangling sound,
  ring-buffer, or callback references.
