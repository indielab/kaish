# 散/集 (Scatter/Gather) — Parallel Processing

Fan-out parallelism for processing items concurrently.

## Basic Syntax

```bash
input | scatter as=VAR limit=N | process $VAR | gather
```

- **散 (scatter)** — fan out input to parallel workers
- **集 (gather)** — collect results back

## Scatter Parameters

| Parameter | Description | Default |
|-----------|-------------|---------|
| `as=VAR` | Variable name for each item | `ITEM` |
| `limit=N` | Max concurrent workers | 8 |

## Gather Parameters

| Parameter | Description | Default |
|-----------|-------------|---------|
| `first=N` | Take first N results only (0 = all) | 0 |
| `format=FMT` | Output format: `lines` or `json` | `lines` |

## Examples

### Process Files in Parallel

```bash
glob "*.json" | scatter as=FILE limit=4 | jq ".name" $FILE | gather
```

### Parallel Computation

```bash
seq 1 20 | scatter as=N limit=4 | echo "result: $((N * N))" | gather
```

### First N Results

```bash
seq 1 100 | scatter as=N limit=8 | echo "done $N" | gather first=10
```

### Transform Data to JSON

```bash
seq 1 10 \
    | scatter as=N limit=4 \
    | echo "{\"id\": $N, \"square\": $((N * N))}" \
    | gather format=json
```

### Pipe Gathered Results

```bash
seq 1 10 \
    | scatter as=N limit=4 \
    | echo "{\"id\": $N, \"square\": $((N * N))}" \
    | gather \
    | jq -s '.'
```

## How It Works

1. **Scatter** reads input line-by-line
2. Each line is assigned to a worker (up to `limit` concurrent)
3. Workers execute the pipeline with `$VAR` set to their item
4. **Gather** collects results as workers complete

## Important Notes

- Results arrive in **completion order**, not input order
- Each worker runs the full pipeline independently
- Variables from the outer scope are visible to workers
- Workers share the same VFS

## Error Handling

- If a worker fails, its error is collected
- Other workers continue
- Gather reports all errors at the end
- Use `set -e` before scatter to stop on first error

## Performance Tips

- Default `limit=8` works well for mixed workloads
- CPU-bound tasks: limit to number of cores
- I/O-bound tasks: higher limits often help
