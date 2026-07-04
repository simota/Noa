#!/usr/bin/env bash
# Monitor CPU and RSS usage for noa and comparable terminal apps.
set -uo pipefail

VERSION="0.1.0"
DEFAULT_PROCESSES=("noa" "Ghostty")

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

Monitor CPU percent and RSS memory for terminal processes. By default this
samples noa and Ghostty. Use --process or positional process names to replace
the default targets.

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
  scripts/monitor-resources.sh --process noa --process Ghostty --output resources.jsonl --json
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

read_process_table() {
  local output

  if output="$(ps -axo comm=,pcpu=,rss= 2>/dev/null)"; then
    printf '%s\n' "$output"
    return 0
  fi

  if output="$(ps -axo comm=,%cpu=,rss= 2>/dev/null)"; then
    printf '%s\n' "$output"
    return 0
  fi

  if output="$(ps -eo comm=,pcpu=,rss= 2>/dev/null)"; then
    printf '%s\n' "$output"
    return 0
  fi

  if output="$(ps -eo comm=,%cpu=,rss= 2>/dev/null)"; then
    printf '%s\n' "$output"
    return 0
  fi

  return 1
}

collect_rows() {
  local targets
  targets="$(join_targets)"

  read_process_table | awk -v targets="$targets" '
    BEGIN {
      n = split(targets, target, "\t")
      for (i = 1; i <= n; i++) {
        cpu[i] = 0
        rss[i] = 0
        count[i] = 0
      }
    }
    {
      if (NF < 3) {
        next
      }

      row = $0
      rss_value = $NF
      cpu_value = $(NF - 1)

      if (rss_value !~ /^[0-9]+$/ || cpu_value !~ /^[0-9]+([.][0-9]+)?$/) {
        next
      }

      command = row
      sub(/[[:space:]]+[0-9]+([.][0-9]+)?[[:space:]]+[0-9]+[[:space:]]*$/, "", command)
      gsub(/^[[:space:]]+|[[:space:]]+$/, "", command)

      base = command
      sub(/^.*\//, "", base)

      lower_command = tolower(command)
      lower_base = tolower(base)

      for (i = 1; i <= n; i++) {
        lower_target = tolower(target[i])
        if (lower_command == lower_target || lower_base == lower_target) {
          cpu[i] += cpu_value
          rss[i] += rss_value
          count[i] += 1
        }
      }
    }
    END {
      for (i = 1; i <= n; i++) {
        status = count[i] > 0 ? "running" : "missing"
        printf "%s\t%.1f\t%d\t%d\t%s\n", target[i], cpu[i], rss[i], count[i], status
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
  emit_line "$(printf '%-20s %6s %-24s %-8s %5s %8s %12s' "timestamp" "sample" "process" "status" "procs" "cpu%" "rss")"
}

emit_human_rows() {
  local timestamp="$1"
  local sample="$2"
  local rows="$3"
  local process cpu rss count status rss_display

  while IFS=$'\t' read -r process cpu rss count status; do
    [ -n "$process" ] || continue
    rss_display="$(rss_human "$rss")"
    emit_line "$(printf '%-20s %6s %-24s %-8s %5s %8s %12s' "$timestamp" "$sample" "$process" "$status" "$count" "$cpu" "$rss_display")"
  done <<< "$rows"
}

emit_json_sample() {
  local timestamp="$1"
  local sample="$2"
  local rows="$3"
  local first=1
  local json
  local process cpu rss count status rss_bytes escaped_process

  json="{\"timestamp\":\"$(json_escape "$timestamp")\",\"sample\":$sample,\"processes\":["

  while IFS=$'\t' read -r process cpu rss count status; do
    [ -n "$process" ] || continue
    [ "$first" -eq 1 ] || json="${json},"
    first=0
    rss_bytes=$((rss * 1024))
    escaped_process="$(json_escape "$process")"
    json="${json}{\"name\":\"$escaped_process\",\"status\":\"$status\",\"pid_count\":$count,\"cpu_percent\":$cpu,\"rss_kib\":$rss,\"rss_bytes\":$rss_bytes}"
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
