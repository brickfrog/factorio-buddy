# Bug: TTS should not block command execution

**STATUS: FIXED**

## Root Cause
Both the macOS `say` backend and OpenAI TTS backend were using blocking calls (`.status().await` and `.wait().await`) that waited for audio playback to complete.

## Fix
Updated `src/cli/say.rs`:
- macOS `say`: Changed to `.spawn()` without waiting
- OpenAI TTS: Wrapped entire pipeline in `tokio::spawn()` to run in background

## Command
```bash
./target/release/factorioctl --host localhost --port 27016 --password test_password say "Message to speak"
```

## Expected Behavior
The `say` command should return immediately after queuing the TTS, allowing the agent to continue with other commands while speech plays in the background.

## Actual Behavior
The command appears to block or cause delays while TTS is playing, slowing down the agent's ability to multitask.

## Error Output
N/A - not an error, but a performance/UX issue.

## Context
- During gameplay sessions where continuous commentary is desired
- Agent needs to speak and act simultaneously

## Workaround
None currently - accept the delay or reduce frequency of TTS messages.
