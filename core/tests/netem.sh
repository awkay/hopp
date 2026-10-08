#!/usr/bin/env bash
# Emulates a good / medium / bad / 3G connection, or an outage, with macOS's traffic shaper
# (dummynet: dnctl pipes + pf rules). Switching profiles mid-call takes effect immediately.
#
#   sudo core/tests/netem.sh bad                    # local LiveKit (lo0, ports 7880-7882)
#   sudo core/tests/netem.sh 3g --remote HOST       # a real call: traffic to/from HOST
#   sudo core/tests/netem.sh drop --for 10          # 10 s outage, then back to what was set
#   sudo core/tests/netem.sh off
#   sudo core/tests/netem.sh status
#
#   profile  up / down           delay (each way)  loss
#   good     20 / 50 Mbit/s      10 ms             0
#   medium   3 / 8 Mbit/s        40 ms             0.5%
#   bad      1 / 2 Mbit/s        75 ms             2%
#   3g       750 / 1600 Kbit/s   150 ms            1%
#   drop     -                   -                 100%
#
# "up" is traffic to the server (a sharer's video), "down" traffic from it (what viewers get).
# Only touches its own pf anchor (com.apple/hopp-netem, loaded by the stock pf.conf) and its own
# pipes (41001 up, 41002 down); `off` removes just those.
set -euo pipefail

ANCHOR="com.apple/hopp-netem"
UP_PIPE=41001
DOWN_PIPE=41002
STATE_FILE=/var/run/hopp-netem.state
LIVEKIT_PORTS="{ 7880 7881 7882 }"

usage() {
  sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'
  exit "${1:-0}"
}

# up-bandwidth down-bandwidth delay-ms loss-rate ("-": unlimited)
profile_settings() {
  case "$1" in
    good) echo "20Mbit/s 50Mbit/s 10 0" ;;
    medium) echo "3Mbit/s 8Mbit/s 40 0.005" ;;
    bad) echo "1Mbit/s 2Mbit/s 75 0.02" ;;
    3g) echo "750Kbit/s 1600Kbit/s 150 0.01" ;;
    drop) echo "- - 0 1" ;;
    *) return 1 ;;
  esac
}

# State: "<profile> <target> <pf token>"; target is "local" or a host.
state_profile="" state_target="" state_token=""
read_state() {
  if [ -f "$STATE_FILE" ]; then
    read -r state_profile state_target state_token < "$STATE_FILE" || true
  fi
}

pf_rules() {
  local target="$1"
  if [ "$target" = "local" ]; then
    echo "dummynet out on lo0 proto { tcp udp } from any to any port $LIVEKIT_PORTS pipe $UP_PIPE"
    echo "dummynet out on lo0 proto { tcp udp } from any port $LIVEKIT_PORTS to any pipe $DOWN_PIPE"
  else
    local iface
    iface="$(route -n get default | awk '/interface:/ {print $2}')"
    echo "dummynet out on $iface from any to $target pipe $UP_PIPE"
    echo "dummynet in on $iface from $target to any pipe $DOWN_PIPE"
  fi
}

config_pipe() {
  local pipe="$1" bandwidth="$2" delay="$3" loss="$4"
  local args=(pipe "$pipe" config delay "$delay" plr "$loss")
  [ "$bandwidth" != "-" ] && args+=(bw "$bandwidth")
  dnctl "${args[@]}"
}

apply() {
  local profile="$1" target="$2"
  local up down delay loss
  read -r up down delay loss <<< "$(profile_settings "$profile")"
  config_pipe "$UP_PIPE" "$up" "$delay" "$loss"
  config_pipe "$DOWN_PIPE" "$down" "$delay" "$loss"

  read_state
  if [ "$state_target" != "$target" ] || [ -z "$state_token" ]; then
    pf_rules "$target" | pfctl -q -a "$ANCHOR" -f -
  fi
  local token="$state_token"
  if [ -z "$token" ]; then
    token="$(pfctl -E 2>&1 | awk '/Token/ {print $NF}')"
  fi
  echo "$profile $target $token" > "$STATE_FILE"
  echo "netem: $profile ($target)"
}

off() {
  read_state
  pfctl -q -a "$ANCHOR" -F all 2>/dev/null || true
  dnctl -q pipe delete "$UP_PIPE" "$DOWN_PIPE" 2>/dev/null || true
  [ -n "$state_token" ] && pfctl -q -X "$state_token" 2>/dev/null || true
  rm -f "$STATE_FILE"
  echo "netem: off"
}

status() {
  read_state
  if [ -z "$state_profile" ]; then
    echo "netem: off"
    return
  fi
  echo "netem: $state_profile ($state_target)"
  dnctl pipe show "$UP_PIPE" "$DOWN_PIPE" 2>/dev/null || true
  pfctl -a "$ANCHOR" -s dummynet 2>/dev/null || true
}

[ $# -ge 1 ] || usage 1
case "$1" in -h | --help) usage ;; esac
if [ "$(id -u)" -ne 0 ]; then
  echo "netem: needs root (sudo $0 $*)" >&2
  exit 1
fi

command="$1"
shift
target="local"
duration=""
while [ $# -gt 0 ]; do
  case "$1" in
    --remote) target="$2"; shift 2 ;;
    --for) duration="$2"; shift 2 ;;
    *) usage 1 ;;
  esac
done

case "$command" in
  off) off ;;
  status) status ;;
  *)
    profile_settings "$command" > /dev/null || usage 1
    if [ -z "$duration" ]; then
      apply "$command" "$target"
      exit 0
    fi
    # Temporary: restore whatever was set before when the time is up (or on Ctrl-C).
    read_state
    previous_profile="$state_profile"
    previous_target="$state_target"
    restore() {
      if [ -n "$previous_profile" ]; then
        apply "$previous_profile" "$previous_target"
      else
        off
      fi
    }
    trap 'restore; exit 130' INT TERM
    apply "$command" "$target"
    sleep "$duration"
    trap - INT TERM
    restore
    ;;
esac
