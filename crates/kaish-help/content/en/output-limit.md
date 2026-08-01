# Output Size Limits

Caps command output size. Output over the cap is written to a spill file, and the result becomes a head preview, a tail preview, and the spill file's path — read that file to get the rest. One case writes only the tail: see "Tail-Only Spills".

## Modes

| Mode | Enabled by default | Limit | Head | Tail |
|------|---------|-------|------|------|
| REPL | off | unlimited | 1K | 512 |
| Agent | on | 8K | 1K | 512 |

The REPL starts with no limit but `set -o output-limit` works at any time. To make it persistent, add it to `~/.kaishrc`.

## How It Works

1. Command runs and produces output
2. If output exceeds `max-bytes`, the captured output is written to a spill file
3. Result is replaced with: head preview + `...` + tail preview + pointer to the spill file
4. Agent can read the spill file selectively with `cat` or `head`/`tail`

## Exit Code on Spill

Spill always exits **3**, and so does an external command that overflows the 10MB capture buffer, even with `output-limit` off. The spill file path is shown in the output. To read it without hitting the limit again:

```sh
set +o output-limit
cat /run/user/1000/kaish/spill/spill-1234567890.123-4567.txt
set -o output-limit=8K
```

## Spill Files

Location: `$XDG_RUNTIME_DIR/kaish/spill/` (typically `/run/user/$UID/kaish/spill/`)

- RAM-backed tmpfs on systemd systems
- Cleared on reboot
- User-scoped (no permission issues)

## kaish-output-limit Builtin

```sh
kaish-output-limit                    # show current config
kaish-output-limit set 64K            # set limit (K/M suffixes or raw bytes)
kaish-output-limit on                 # enable with default 8K limit
kaish-output-limit off                # disable (unlimited)
kaish-output-limit head 2048          # set head preview size
kaish-output-limit tail 1024          # set tail preview size
```

## Truncated Output Format

```
<first 1024 bytes of output>
...
<last 512 bytes of output>
[output truncated: 234567 bytes total — full output at /run/user/1000/kaish/spill/spill-1234567890.123-4567.txt]
```

`234567 bytes total` is the whole output, and the spill file holds all of it.

## Tail-Only Spills

An external command's stdout is captured in a fixed 10MB (10485760-byte) buffer, whatever `max-bytes` says. Output past 10MB drops the earliest bytes before the spill runs, so the spill file holds the tail only. The message says `captured` in place of `total` to mark that the count is what survived, not the size of the output:

```
<first 1024 bytes of output>
...
<last 512 bytes of output>
[output truncated: 10485760 bytes captured — tail only at /run/user/1000/kaish/spill/spill-1234567890.123-4567.txt; earlier output was dropped before the spill, so see the stderr overflow marker for the true size]
```

The overflow marker on stderr gives the true numbers — bytes lost and bytes written:

```
[stdout truncated: output exceeded the 10MB capture buffer — first 4194304 bytes lost (14680064 bytes total written); enable output-limit to spill to disk]
```

The lost bytes are gone; rerun with `grep`, `head`, or a redirect to a file to get the earlier output.
