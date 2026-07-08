#!/usr/bin/env bash
# Monitor CPU, memory, and scheduling metrics for Noa and comparable terminal apps.
set -uo pipefail

VERSION="0.2.0"
DEFAULT_PROCESSES=("Noa" "Ghostty" "Terminal" "iTerm2")

INTERVAL="2"
SAMPLES="0"
JSON_MODE=0
OUTPUT_FILE=""
EXPLICIT_PROCESSES=()
PROCESSES=()

print_help() {
  cat <<'EOF'
Usage:
  scripts/monitor-resources.sh [options] [process ...]

Monitor CPU percent, RSS, memory footprint, thread count, and idle wakeups for
terminal processes. By default this samples Noa, Ghostty, Terminal, and iTerm2.
Use --process or positional process names to replace the default targets.

Options:
  -p, --process NAME     Add a process command name to monitor. May be repeated.
  -i, --interval SECONDS Sampling interval. Defaults to 2. Allows 0 for tests.
  -s, --samples COUNT    Number of samples to take. Defaults to 0 (until Ctrl+C).
  -o, --output FILE      Write output to FILE instead of stdout. Use - for stdout.
      --json             Emit JSON Lines instead of a human-readable table.
  -h, --help             Show this help.
  -V, --version          Show version.

Output:
  Human mode prints one table row per process per sample.
  JSON mode prints one JSON object per sample with a processes array.
  Missing processes are reported with cpu_percent=0, rss_kib=0, and status=missing.

Metrics:
  cpu%, rss        From ps. RSS counts shared pages, so it overstates real cost.
  footprint        Physical memory footprint from top (what Activity Monitor
                   shows as "Memory"); the right metric for real memory cost.
  th               Thread count.
  idlew            Idle wakeups, cumulative since process start. Diff between
                   samples to get a rate; lower is better for battery.
  Footprint, th, and idlew are 0 when the top command is unavailable or fails
  (e.g. sandboxed environments); cpu%/rss still come from ps.

Exit codes:
  0    Completed successfully, including when targets are missing.
  1    Runtime error such as an unreadable process table or output write failure.
  2    Usage error.
  127  Required system command is missing.
  130  Interrupted by Ctrl+C.
  143  Terminated by SIGTERM.

Examples:
  scripts/monitor-resources.sh
  scripts/monitor-resources.sh --samples 5 --interval 1
  scripts/monitor-resources.sh --json --samples 1 --interval 0
  scripts/monitor-resources.sh --process Noa --process Ghostty --output resources.jsonl --json
  scripts/monitor-resources.sh wezterm Alacritty
EOF
}

print_version() {
  printf 'monitor-resources.sh %s\n' "$VERSION"
}

usage_error() {
  printf 'error: %s\n' "$1" >&2
  printf 'Run scripts/monitor-resources.sh --help for usage.\n' >&2
  exit 2
}

runtime_error() {
  printf 'error: %s\n' "$1" >&2
  exit "${2:-1}"
}

on_int() {
  trap - INT
  printf '\ninterrupted\n' >&2
  exit 130
}

on_term() {
  trap - TERM
  printf '\nterminated\n' >&2
  exit 143
}

trap on_int INT
trap on_term TERM

is_non_negative_integer() {
  case "$1" in
    ''|*[!0-9]*) return 1 ;;
    *) return 0 ;;
  esac
}

is_non_negative_number() {
  [[ "$1" =~ ^([0-9]+([.][0-9]+)?|[.][0-9]+)$ ]]
}

add_process() {
  [ -n "$1" ] || usage_error "process name cannot be empty"
  EXPLICIT_PROCESSES+=("$1")
}

parse_args() {
  while [ "$#" -gt 0 ]; do
    case "$1" in
      -h|--help)
        print_help
        exit 0
        ;;
      -V|--version)
        print_version
        exit 0
        ;;
      -p|--process)
        [ "$#" -ge 2 ] || usage_error "$1 requires a process name"
        shift
        add_process "$1"
        ;;
      --process=*)
        add_process "${1#--process=}"
        ;;
      -i|--interval)
        [ "$#" -ge 2 ] || usage_error "$1 requires a value"
        shift
        INTERVAL="$1"
        ;;
      --interval=*)
        INTERVAL="${1#--interval=}"
        ;;
      -s|--samples)
        [ "$#" -ge 2 ] || usage_error "$1 requires a value"
        shift
        SAMPLES="$1"
        ;;
      --samples=*)
        SAMPLES="${1#--samples=}"
        ;;
      -o|--output)
        [ "$#" -ge 2 ] || usage_error "$1 requires a file path"
        shift
        OUTPUT_FILE="$1"
        ;;
      --output=*)
        OUTPUT_FILE="${1#--output=}"
        ;;
      --json)
        JSON_MODE=1
        ;;
      --)
        shift
        while [ "$#" -gt 0 ]; do
          add_process "$1"
          shift
        done
        break
        ;;
      -*)
        usage_error "unknown option: $1"
        ;;
      *)
        add_process "$1"
        ;;
    esac
    shift
  done

  is_non_negative_number "$INTERVAL" || usage_error "--interval must be a non-negative number"
  is_non_negative_integer "$SAMPLES" || usage_error "--samples must be a non-negative integer"

  if [ "${#EXPLICIT_PROCESSES[@]}" -eq 0 ]; then
    PROCESSES=("${DEFAULT_PROCESSES[@]}")
  else
    dedupe_processes
  fi
}

dedupe_processes() {
  local process existing found
  PROCESSES=()
  for process in "${EXPLICIT_PROCESSES[@]}"; do
    found=0
    for existing in "${PROCESSES[@]}"; do
      if [ "$existing" = "$process" ]; then
        found=1
        break
      fi
    done
    [ "$found" -eq 1 ] || PROCESSES+=("$process")
  done
}

require_commands() {
  command -v ps >/dev/null 2>&1 || runtime_error "required command not found: ps" 127
  command -v awk >/dev/null 2>&1 || runtime_error "required command not found: awk" 127
  command -v date >/dev/null 2>&1 || runtime_error "required command not found: date" 127
  command -v sleep >/dev/null 2>&1 || runtime_error "required command not found: sleep" 127
}

prepare_output() {
  [ -z "$OUTPUT_FILE" ] && return 0
  [ "$OUTPUT_FILE" = "-" ] && return 0

  : > "$OUTPUT_FILE" 2>/dev/null || runtime_error "cannot write output file: $OUTPUT_FILE" 1
}

emit_line() {
  if [ -n "$OUTPUT_FILE" ] && [ "$OUTPUT_FILE" != "-" ]; then
    printf '%s\n' "$1" >> "$OUTPUT_FILE" || runtime_error "failed writing output file: $OUTPUT_FILE" 1
  else
    printf '%s\n' "$1" || runtime_error "failed writing to stdout" 1
  fi
}

join_targets() {
  local first=1 process
  for process in "${PROCESSES[@]}"; do
    if [ "$first" -eq 1 ]; then
      printf '%s' "$process"
      first=0
    else
      printf '\t%s' "$process"
    fi
  done
}

read_process_table_comm() {
  local output

  if output="$(ps -axo pid=,comm=,pcpu=,rss= 2>/dev/null)"; then
    printf '%s\n' "$output"
    return 0
  fi

  if output="$(ps -axo pid=,comm=,%cpu=,rss= 2>/dev/null)"; then
    printf '%s\n' "$output"
    return 0
  fi

  if output="$(ps -eo pid=,comm=,pcpu=,rss= 2>/dev/null)"; then
    printf '%s\n' "$output"
    return 0
  fi

  if output="$(ps -eo pid=,comm=,%cpu=,rss= 2>/dev/null)"; then
    printf '%s\n' "$output"
    return 0
  fi

  return 1
}

read_process_table_ucomm() {
  local output

  if output="$(ps -axo pid=,ucomm=,pcpu=,rss= 2>/dev/null)"; then
    printf '%s\n' "$output"
    return 0
  fi

  if output="$(ps -axo pid=,ucomm=,%cpu=,rss= 2>/dev/null)"; then
    printf '%s\n' "$output"
    return 0
  fi

  if output="$(ps -eo pid=,ucomm=,pcpu=,rss= 2>/dev/null)"; then
    printf '%s\n' "$output"
    return 0
  fi

  if output="$(ps -eo pid=,ucomm=,%cpu=,rss= 2>/dev/null)"; then
    printf '%s\n' "$output"
    return 0
  fi

  return 1
}

# Per-process footprint / threads / idle wakeups. Only top exposes these
# without root; absence is tolerated (fields report 0).
read_top_table() {
  command -v top >/dev/null 2>&1 || return 1
  top -l 1 -stats pid,th,idlew,mem 2>/dev/null
}

read_process_table() {
  local comm_output ucomm_output

  if comm_output="$(read_process_table_comm)"; then
    printf '%s\n' "$comm_output"

    if ucomm_output="$(read_process_table_ucomm)"; then
      printf '__NOA_MONITOR_UCOMM_FALLBACK__\n'
      printf '%s\n' "$ucomm_output"
    fi

    return 0
  fi

  read_process_table_ucomm
}

collect_rows() {
  local targets top_output
  targets="$(join_targets)"

  {
    read_process_table
    if top_output="$(read_top_table)"; then
      printf '__NOA_MONITOR_TOP__\n'
      printf '%s\n' "$top_output"
    fi
  } | awk -v targets="$targets" '
    # Converts top MEM values like "12M+", "2912K", "0B" to KiB.
    function mem_to_kib(value,    number, unit) {
      gsub(/[+\-]/, "", value)
      unit = value
      sub(/^[0-9.]+/, "", unit)
      number = value + 0
      if (unit == "B") return int(number / 1024)
      if (unit == "M") return int(number * 1024)
      if (unit == "G") return int(number * 1024 * 1024)
      return int(number)
    }
    BEGIN {
      n = split(targets, target, "\t")
      source = "primary"
      for (i = 1; i <= n; i++) {
        cpu[i] = 0
        rss[i] = 0
        count[i] = 0
        footprint[i] = 0
        threads[i] = 0
        idlew[i] = 0
      }
    }
    /^__NOA_MONITOR_UCOMM_FALLBACK__$/ {
      source = "fallback"
      next
    }
    /^__NOA_MONITOR_TOP__$/ {
      source = "top"
      next
    }
    source == "top" {
      # top rows: PID  #TH  IDLEW  MEM (skip the summary header block).
      if (NF < 4 || $1 !~ /^[0-9]+$/) {
        next
      }
      if (!($1 in pid_targets)) {
        next
      }
      th_value = $2
      sub(/\/.*$/, "", th_value)
      idlew_value = $3
      gsub(/[^0-9]/, "", idlew_value)
      kib = mem_to_kib($4)
      m = split(pid_targets[$1], hits, ",")
      for (h = 1; h <= m; h++) {
        i = hits[h]
        threads[i] += th_value + 0
        idlew[i] += idlew_value + 0
        footprint[i] += kib
      }
      next
    }
    {
      if (NF < 4 || $1 !~ /^[0-9]+$/) {
        next
      }

      row = $0
      pid = $1
      rss_value = $NF
      cpu_value = $(NF - 1)

      if (rss_value !~ /^[0-9]+$/ || cpu_value !~ /^[0-9]+([.][0-9]+)?$/) {
        next
      }

      command = row
      sub(/^[[:space:]]*[0-9]+[[:space:]]+/, "", command)
      sub(/[[:space:]]+[0-9]+([.][0-9]+)?[[:space:]]+[0-9]+[[:space:]]*$/, "", command)
      gsub(/^[[:space:]]+|[[:space:]]+$/, "", command)

      base = command
      sub(/^.*\//, "", base)

      lower_command = tolower(command)
      lower_base = tolower(base)

      for (i = 1; i <= n; i++) {
        lower_target = tolower(target[i])
        if (lower_command == lower_target || lower_base == lower_target) {
          if ((i, pid) in seen) {
            continue
          }
          seen[i, pid] = 1
          cpu[i] += cpu_value
          rss[i] += rss_value
          count[i] += 1
          pid_targets[pid] = (pid in pid_targets) ? pid_targets[pid] "," i : i
        }
      }
    }
    END {
      for (i = 1; i <= n; i++) {
        status = count[i] > 0 ? "running" : "missing"
        printf "%s\t%.1f\t%d\t%d\t%s\t%d\t%d\t%d\n", target[i], cpu[i], rss[i], count[i], status, footprint[i], threads[i], idlew[i]
      }
    }
  '
}

json_escape() {
  local value="$1"
  value="${value//\\/\\\\}"
  value="${value//\"/\\\"}"
  value="${value//$'\n'/\\n}"
  value="${value//$'\r'/\\r}"
  value="${value//$'\t'/\\t}"
  printf '%s' "$value"
}

rss_human() {
  local kib="$1"

  if [ "$kib" -le 0 ]; then
    printf '0 B'
  elif [ "$kib" -lt 1024 ]; then
    printf '%d KiB' "$kib"
  elif [ "$kib" -lt 1048576 ]; then
    awk -v kib="$kib" 'BEGIN { printf "%.1f MiB", kib / 1024 }'
  else
    awk -v kib="$kib" 'BEGIN { printf "%.2f GiB", kib / 1048576 }'
  fi
}

emit_human_header() {
  emit_line "$(printf '%-20s %6s %-24s %-8s %5s %8s %12s %12s %5s %8s' "timestamp" "sample" "process" "status" "procs" "cpu%" "rss" "footprint" "th" "idlew")"
}

emit_human_rows() {
  local timestamp="$1"
  local sample="$2"
  local rows="$3"
  local process cpu rss count status footprint threads idlew rss_display footprint_display

  while IFS=$'\t' read -r process cpu rss count status footprint threads idlew; do
    [ -n "$process" ] || continue
    rss_display="$(rss_human "$rss")"
    footprint_display="$(rss_human "$footprint")"
    emit_line "$(printf '%-20s %6s %-24s %-8s %5s %8s %12s %12s %5s %8s' "$timestamp" "$sample" "$process" "$status" "$count" "$cpu" "$rss_display" "$footprint_display" "$threads" "$idlew")"
  done <<< "$rows"
}

emit_json_sample() {
  local timestamp="$1"
  local sample="$2"
  local rows="$3"
  local first=1
  local json
  local process cpu rss count status footprint threads idlew rss_bytes footprint_bytes escaped_process

  json="{\"timestamp\":\"$(json_escape "$timestamp")\",\"sample\":$sample,\"processes\":["

  while IFS=$'\t' read -r process cpu rss count status footprint threads idlew; do
    [ -n "$process" ] || continue
    [ "$first" -eq 1 ] || json="${json},"
    first=0
    rss_bytes=$((rss * 1024))
    footprint_bytes=$((footprint * 1024))
    escaped_process="$(json_escape "$process")"
    json="${json}{\"name\":\"$escaped_process\",\"status\":\"$status\",\"pid_count\":$count,\"cpu_percent\":$cpu,\"rss_kib\":$rss,\"rss_bytes\":$rss_bytes,\"phys_footprint_kib\":$footprint,\"phys_footprint_bytes\":$footprint_bytes,\"thread_count\":$threads,\"idle_wakeups\":$idlew}"
  done <<< "$rows"

  json="${json}]}"
  emit_line "$json"
}

sample_once() {
  local sample="$1"
  local timestamp rows

  timestamp="$(date -u '+%Y-%m-%dT%H:%M:%SZ')"
  rows="$(collect_rows)" || runtime_error "failed to read process table with ps" 1

  if [ "$JSON_MODE" -eq 1 ]; then
    emit_json_sample "$timestamp" "$sample" "$rows"
  else
    emit_human_rows "$timestamp" "$sample" "$rows"
  fi
}

run_monitor() {
  local sample=1

  if [ "$JSON_MODE" -eq 0 ]; then
    emit_human_header
  fi

  while :; do
    sample_once "$sample"

    if [ "$SAMPLES" -gt 0 ] && [ "$sample" -ge "$SAMPLES" ]; then
      break
    fi

    sample=$((sample + 1))
    sleep "$INTERVAL"
  done
}

main() {
  parse_args "$@"
  require_commands
  prepare_output
  run_monitor
}

main "$@"
