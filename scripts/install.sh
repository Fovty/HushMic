#!/bin/sh
# hushmic installer — portable POSIX sh.
#
# Two modes, auto-detected:
#   * Co-located: when this script sits inside an extracted release tarball
#     (next to bin/hushmic, lib/, share/), it installs those files.
#   * Standalone: otherwise it downloads the latest release tarball from GitHub,
#     extracts it, and installs from there.
#
# Usage:
#   install.sh [--prefix DIR] [--no-check]   install (default prefix /usr;
#                                            sudo where needed)
#   install.sh --check          only check the sound system (exit 1: missing)
#   install.sh --uninstall [--prefix DIR]
#   install.sh -h | --help
#
# Before installing, it checks that PipeWire (with its command line tools),
# WirePlumber and pipewire-pulse are installed and run in your session, and
# says what to install and enable where they don't. It never installs a
# package or changes a service itself.
set -eu

# Pin a known umask: a hardened caller umask (077/027 — common on CIS/STIG
# boxes, and preserved across sudo on some distros) would otherwise make the
# root-created dirs and copied libs unreadable to the user-session PipeWire
# that has to load the plugin and models (install "succeeds", mic is silent).
# Same fix as the Nix installer; the explicit modes below are a second layer.
umask 022

REPO="Fovty/hushmic"
ARCH="x86_64"
# VERSION is derived later from the actual payload (the co-located dir name or
# the latest release tarball), never hardcoded — so this script does not go
# stale when the released version changes.
VERSION=""

PREFIX="/usr"
ACTION="install"
NO_CHECK=""

# ---------------------------------------------------------------------------
# Arg parsing
# ---------------------------------------------------------------------------
usage() {
  cat <<EOF
hushmic installer

Usage:
  $0 [--prefix DIR]          Install (default --prefix /usr); sudo only where
                             this user can't write, and said first
  $0 --check                 Only check the sound system: PipeWire, its tools,
                             WirePlumber and pipewire-pulse, installed and
                             running (exit 1 when something is missing)
  $0 --uninstall [--prefix DIR]
  $0 -h | --help

Before installing, the same check runs; where something is missing it
prints the commands that fix it and asks whether to go on. It installs no
packages and changes no services. --no-check skips it (non-interactive use;
without a terminal to ask, it says what is missing and goes on).

Examples:
  sudo $0                     # system install under /usr
  $0 --prefix "\$HOME/.local"  # user-local install (no root needed)
EOF
}

while [ $# -gt 0 ]; do
  case "$1" in
    --prefix) PREFIX="${2:?--prefix needs an argument}"; shift 2 ;;
    --prefix=*) PREFIX="${1#*=}"; shift ;;
    --uninstall) ACTION="uninstall"; shift ;;
    --check) ACTION="check"; shift ;;
    --no-check) NO_CHECK="yes"; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "error: unknown argument: $1" >&2; usage >&2; exit 2 ;;
  esac
done

# Normalize PREFIX to an absolute path (best-effort).
case "$PREFIX" in
  /*) : ;;
  *) PREFIX="$(pwd)/$PREFIX" ;;
esac

# ---------------------------------------------------------------------------
# Privilege handling
# ---------------------------------------------------------------------------
# Shared with the generated hushmic-uninstall (printed into it verbatim).
PREFIX_DIRS=$(cat <<'PREFIX_DIRS_EOF'
# The first folder under $PREFIX that an install writes into (or makes
# under) and this user cannot write, else nothing. Every one counts, not
# only the prefix: a user-owned /usr can still hold root-owned share/ or
# lib/ladspa, which would leave a half-written install.
first_unwritable() {
  for _d in "$PREFIX" "$PREFIX/bin" "$PREFIX/lib" "$PREFIX/lib/ladspa" \
            "$PREFIX/lib/hushmic" "$PREFIX/lib/systemd/user" "$PREFIX/share" \
            "$PREFIX/share/hushmic" "$PREFIX/share/hushmic/models" \
            "$PREFIX/share/applications" "$PREFIX/share/licenses" \
            "$PREFIX/share/icons/hicolor" "$PREFIX/share/icons/hicolor"/*/apps \
            "$PREFIX/share/icons/hicolor"/*/status; do
    _e="$_d"
    while [ ! -e "$_e" ] && [ "$_e" != / ]; do _e="$(dirname "$_e")"; done
    if [ ! -d "$_e" ] || [ ! -w "$_e" ]; then
      printf '%s\n' "$_e"
      return 0
    fi
  done
  return 1
}
# The layout's folders an uninstall left empty, deepest first; never under
# /usr or /usr/local, whose folders belong to the system.
drop_empty_dirs() {
  case "$PREFIX" in /usr|/usr/|/usr/local|/usr/local/) return 0 ;; esac
  for _d in "$PREFIX/lib/ladspa" "$PREFIX/lib/systemd/user" "$PREFIX/lib/systemd" \
            "$PREFIX/share/applications" "$PREFIX/share/licenses" "$PREFIX/share/icons" \
            "$PREFIX/bin" "$PREFIX/lib" "$PREFIX/share" "$PREFIX"; do
    if [ -d "$_d" ]; then $1 rmdir "$_d" 2>/dev/null || true; fi
  done
}
PREFIX_DIRS_EOF
)
eval "$PREFIX_DIRS"

# Root only where this user can't write (a prefix of one's own, even outside
# $HOME, needs none), and said before it is used.
NEED_SUDO=""
if [ "$(id -u)" -ne 0 ] && [ "$ACTION" != check ]; then
  if _locked="$(first_unwritable)"; then
    if command -v sudo >/dev/null 2>&1; then
      NEED_SUDO="yes"
      echo "using sudo for $PREFIX ($_locked is not writable)"
    else
      echo "error: $_locked is not writable, and sudo was not found." >&2
      echo "       Re-run as root (e.g. 'curl ... | sudo sh') or use --prefix \"\$HOME/.local\"." >&2
      exit 1
    fi
  fi
fi

as_root() {
  if [ -n "$NEED_SUDO" ]; then
    sudo "$@"
  else
    "$@"
  fi
}

# ---------------------------------------------------------------------------
# Install-layout destinations (uniform: PREFIX + layout)
# ---------------------------------------------------------------------------
DEST_BIN="$PREFIX/bin"
DEST_LADSPA="$PREFIX/lib/ladspa"
DEST_LIB="$PREFIX/lib/hushmic"
DEST_MODELS="$PREFIX/share/hushmic/models"
DEST_APPS="$PREFIX/share/applications"
DEST_UNITS="$PREFIX/lib/systemd/user"
DEST_HICOLOR="$PREFIX/share/icons/hicolor"
DEST_LICENSES="$PREFIX/share/licenses/hushmic"

# The app icon in the payload: every hicolor size plus scalable/apps/hushmic.svg;
# must stay in lockstep with packaging/tray/hicolor/.
APP_ICON_SIZES="16x16 22x22 24x24 32x32 48x48 64x64 128x128 256x256 512x512"
# Tray status-icon ladder shipped in the payload (five SNI names x eight
# sizes, status/ context); must stay in lockstep with packaging/tray/hicolor/.
TRAY_SIZES="16x16 22x22 24x24 32x32 48x48 64x64 128x128 256x256"
TRAY_NAMES="hushmic-tray hushmic-tray-off hushmic-tray-bypass hushmic-tray-mute hushmic-tray-error"
# The monochrome set the desktop recolors: the same five states under the same
# stems plus a -symbolic suffix, in GNOME's and Plasma's panel sizes (with their
# @2 twins) plus scalable/ for everything else.
TRAY_SYMBOLIC_DIRS="16x16 16x16@2 22x22 22x22@2 24x24 24x24@2 scalable"

prune_icon_dirs() {
  # Drop the icon directories the removal left empty (rmdir only takes empty
  # ones). Never under /usr, where hicolor-icon-theme owns them.
  case "$PREFIX" in /usr|/usr/) return 0 ;; esac
  for _d in "$DEST_HICOLOR"/*/apps "$DEST_HICOLOR"/*/status "$DEST_HICOLOR"/* "$DEST_HICOLOR"; do
    if [ -d "$_d" ]; then as_root rmdir "$_d" 2>/dev/null || true; fi
  done
}

refresh_icon_cache() {
  # Refresh the hicolor cache so SNI hosts pick icon changes up without a
  # re-login; a missing tool or a cache-less prefix (no index.theme — normal
  # for a fresh ~/.local) is fine, hence the '|| true'.
  if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    as_root gtk-update-icon-cache -f "$DEST_HICOLOR" >/dev/null 2>&1 || true
  fi
}

# ---------------------------------------------------------------------------
# Uninstall
# ---------------------------------------------------------------------------
# The invoking user's side of an uninstall, shared with the generated
# hushmic-uninstall (printed into it verbatim): stop a running HushMic, put
# back a default microphone a crashed run left on hushmic_source (gone with
# the install), and remove HushMic's state and cache. The settings stay.
USER_CLEANUP=$(cat <<'USER_CLEANUP_EOF'
# Run a command as the invoking user, in their session (not root's).
user_run() {
  if [ -n "${SUDO_USER:-}" ]; then
    _uid="$(id -u "$SUDO_USER" 2>/dev/null)" || return 1
    sudo -u "$SUDO_USER" XDG_RUNTIME_DIR="/run/user/$_uid" "$@"
  else
    "$@"
  fi
}
user_dirs() {
  if [ -n "${SUDO_USER:-}" ]; then
    _home="$(getent passwd "$SUDO_USER" 2>/dev/null | cut -d: -f6)"
    _state="${_home:+$_home/.local/state}"
    _cache="${_home:+$_home/.cache}"
  else
    _state="${XDG_STATE_HOME:-${HOME:-}/.local/state}"
    _cache="${XDG_CACHE_HOME:-${HOME:-}/.cache}"
    _home="${HOME:-}"
  fi
}
# The unit `hushmic service install` generated ($1): removed when it runs
# this install's binary, else only named (it runs another HushMic).
remove_generated_unit() {
  [ -f "$1" ] || return 0
  # ours: ExecStart is exactly this install's binary (bare, or quoted as
  # `hushmic service install` writes it), not hushmic.AppImage beside it
  _bin="$PREFIX/bin/hushmic"
  _ours=""
  while IFS= read -r _l || [ -n "$_l" ]; do
    case "$_l" in
      "ExecStart=$_bin"|"ExecStart=$_bin "*|"ExecStart=\"$_bin\""|"ExecStart=\"$_bin\" "*) _ours=yes ;;
    esac
  done < "$1"
  if [ -n "$_ours" ]; then
    rm -f "$1"
    user_run systemctl --user daemon-reload >/dev/null 2>&1 || true
    echo "Removed the systemd user unit $1."
  else
    echo "A systemd user unit for another HushMic stays: $1"
  fi
}
# $1: the hushmic binary about to be removed.
user_cleanup() {
  user_dirs
  # A running HushMic gives the default microphone back as it stops.
  if [ -x "$1" ]; then user_run "$1" quit >/dev/null 2>&1 || true; fi
  if command -v pw-metadata >/dev/null 2>&1 &&
     user_run pw-metadata -n default 0 default.configured.audio.source 2>/dev/null |
       grep -q '"hushmic_source"'; then
    _prior=""
    for _f in "$_state/hushmic/prior-default" "$_home/.config/hushmic/prior-default"; do
      if [ -s "$_f" ]; then _prior="$(cat "$_f")"; break; fi
    done
    if [ -n "$_prior" ] && [ "$_prior" != hushmic_source ]; then
      # JSON: the name's backslashes and quotes escaped, one argument
      _json="$(printf '%s' "$_prior" | sed 's/\\/\\\\/g; s/"/\\"/g')"
      if user_run pw-metadata -n default 0 default.configured.audio.source \
           "{ \"name\": \"$_json\" }" Spa:String:JSON >/dev/null 2>&1; then
        echo "The default microphone was left on HushMic's; it is $_prior again."
      else
        echo "The default microphone is still set to HushMic's, which is gone; pick yours in your sound settings."
      fi
    else
      if user_run pw-metadata -n default -d 0 default.configured.audio.source >/dev/null 2>&1; then
        echo "The default microphone was left on HushMic's; your desktop picks one again."
      else
        echo "The default microphone is still set to HushMic's, which is gone; pick yours in your sound settings."
      fi
    fi
  fi
  if [ -n "$_state" ] && [ -d "$_state/hushmic" ]; then rm -rf "$_state/hushmic"; fi
  if [ -n "$_cache" ] && [ -d "$_cache/hushmic" ]; then rm -rf "$_cache/hushmic"; fi
  echo "Removed HushMic's state ($_state/hushmic) and cache ($_cache/hushmic)."
  echo "Your settings in ~/.config/hushmic stay; remove that folder to forget them."
}
USER_CLEANUP_EOF
)
eval "$USER_CLEANUP"

# ---------------------------------------------------------------------------
# Sound system check (issue #30)
# ---------------------------------------------------------------------------
# What HushMic needs from the system: PipeWire with its command line tools,
# WirePlumber and pipewire-pulse, running in the user's session (with sudo:
# the invoking user's). Read only: it says what is missing and the commands
# that fix it, and never installs a package or touches a service.

# Whose session the check reads: SUDO_USER's; as root without it (su -,
# sudo su -, pkexec) the user PKEXEC_UID or logname names; else this one's.
# CHECK_BLIND: root and no user found, so whether things run is unknown.
check_user() {
  CHECK_USER=""
  CHECK_BLIND=""
  CHECK_UID="$(id -u)"
  if [ -n "${SUDO_USER:-}" ] && [ "$SUDO_USER" != root ]; then
    CHECK_USER="$SUDO_USER"
  elif [ "$CHECK_UID" -eq 0 ]; then
    if [ -n "${PKEXEC_UID:-}" ]; then
      CHECK_USER="$(getent passwd "$PKEXEC_UID" 2>/dev/null | cut -d: -f1)"
      [ -n "$CHECK_USER" ] || CHECK_USER="$(id -nu "$PKEXEC_UID" 2>/dev/null)" || CHECK_USER=""
    fi
    if { [ -z "$CHECK_USER" ] || [ "$CHECK_USER" = root ]; } &&
       command -v logname >/dev/null 2>&1; then
      CHECK_USER="$(logname 2>/dev/null)" || CHECK_USER=""
    fi
    [ "$CHECK_USER" != root ] || CHECK_USER=""
  fi
  if [ -n "$CHECK_USER" ]; then
    CHECK_UID="$(id -u "$CHECK_USER" 2>/dev/null)" || { CHECK_USER=""; CHECK_UID="$(id -u)"; }
  fi
  if [ -z "$CHECK_USER" ] && [ "$CHECK_UID" -eq 0 ]; then CHECK_BLIND="yes"; fi
}

# Run a command in that user's session (sudo, else runuser without sudo).
check_run() {
  if [ -z "$CHECK_USER" ]; then
    "$@"
  elif command -v sudo >/dev/null 2>&1; then
    sudo -u "$CHECK_USER" XDG_RUNTIME_DIR="/run/user/$CHECK_UID" "$@"
  else
    runuser -u "$CHECK_USER" -- env XDG_RUNTIME_DIR="/run/user/$CHECK_UID" "$@"
  fi
}

# The distro family by os-release (ID first, then ID_LIKE): debian, fedora,
# arch, suse, or nothing. HUSHMIC_OS_RELEASE: another file (tests).
os_field() {
  sed -n "s/^$1=//p" "${HUSHMIC_OS_RELEASE:-/etc/os-release}" 2>/dev/null |
    head -1 | tr -d "\"'"
}
distro_family() {
  for _w in $(os_field ID) $(os_field ID_LIKE); do
    case "$_w" in
      debian|ubuntu|linuxmint|pop|elementary|zorin|neon|kali|raspbian|devuan)
        echo debian; return 0 ;;
      fedora|rhel|centos|rocky|almalinux|nobara|ultramarine)
        echo fedora; return 0 ;;
      arch|manjaro|endeavouros|cachyos|garuda|artix)
        echo arch; return 0 ;;
      opensuse*|suse|sles|sled)
        echo suse; return 0 ;;
    esac
  done
  # an unknown distro: by its package manager
  if command -v apt-get >/dev/null 2>&1; then echo debian
  elif command -v dnf >/dev/null 2>&1 || command -v rpm-ostree >/dev/null 2>&1; then echo fedora
  elif command -v pacman >/dev/null 2>&1; then echo arch
  elif command -v zypper >/dev/null 2>&1; then echo suse
  fi
}

# The package that brings a part, by family (nothing for an unknown one).
# Parts: daemon (pipewire), tools (pw-cli, pw-dump, pw-metadata, pw-link),
# play (pw-record, pw-play; Arch packs them apart), session (wireplumber),
# pulse (pipewire-pulse).
part_package() {
  case "$1:$2" in
    :*) ;;
    *:daemon) echo pipewire ;;
    *:session) echo wireplumber ;;
    debian:tools|debian:play) echo pipewire-bin ;;
    fedora:tools|fedora:play) echo pipewire-utils ;;
    suse:tools|suse:play) echo pipewire-tools ;;
    arch:tools) echo pipewire ;;
    arch:play) echo pipewire-audio ;;
    debian:pulse|arch:pulse) echo pipewire-pulse ;;
    fedora:pulse|suse:pulse) echo pipewire-pulseaudio ;;
  esac
}

# Whether every command named is on PATH.
have_all() {
  for _c in "$@"; do command -v "$_c" >/dev/null 2>&1 || return 1; done
}

# yes, no or unknown: whether a user service runs ($1: its unit without
# .service, $2: its process name), in the invoking user's session. An
# active socket counts: pipewire-pulse may start only when an app connects.
runs() {
  if check_run systemctl --user is-active --quiet "$1.service" "$1.socket" 2>/dev/null; then
    echo yes; return 0
  fi
  if command -v pgrep >/dev/null 2>&1; then
    if pgrep -u "$CHECK_UID" -x "$2" >/dev/null 2>&1; then echo yes; else echo no; fi
    return 0
  fi
  if check_run systemctl --user show-environment >/dev/null 2>&1; then echo no; else echo unknown; fi
}

# The running PulseAudio server's name as pactl says it ("PulseAudio (on
# PipeWire 1.0.5)" is pipewire-pulse), else nothing.
pulse_server() {
  if command -v pactl >/dev/null 2>&1; then
    check_run env LC_ALL=C pactl info 2>/dev/null | sed -n 's/^Server Name: //p' | head -1
  elif command -v pgrep >/dev/null 2>&1 && pgrep -u "$CHECK_UID" -x pulseaudio >/dev/null 2>&1; then
    echo pulseaudio
  fi
}

# Print the state and, where something is missing, how to fix it. Returns 1
# when something is missing or not running.
sound_check() {
  check_user
  _family="$(distro_family)"
  _missing=""
  _absent=""
  _stopped=""
  _unknown=""
  _add() {
    [ -n "$1" ] || return 0
    case " $_missing " in *" $1 "*) ;; *) _missing="${_missing:+$_missing }$1" ;; esac
  }
  echo "Checking the sound system (--no-check skips this):"
  if [ -n "$CHECK_BLIND" ]; then
    echo "  As root, your session can't be checked: only what is installed is."
  fi
  # label, part, commands, unit, process
  for _row in "PipeWire|daemon|pipewire|pipewire|pipewire" \
              "PipeWire tools|tools|pw-cli pw-dump pw-metadata pw-link pw-record pw-play||" \
              "WirePlumber|session|wireplumber|wireplumber|wireplumber" \
              "pipewire-pulse|pulse|pipewire-pulse|pipewire-pulse|pipewire-pulse"; do
    _label="${_row%%|*}"; _rest="${_row#*|}"
    _part="${_rest%%|*}"; _rest="${_rest#*|}"
    _cmds="${_rest%%|*}"; _rest="${_rest#*|}"
    _unit="${_rest%%|*}"; _proc="${_rest#*|}"
    # shellcheck disable=SC2086 # one word per command
    if ! have_all $_cmds; then
      _state="not installed"
      _absent="${_absent:+$_absent, }$_label"
      if [ "$_part" = tools ]; then
        have_all pw-cli pw-dump pw-metadata pw-link || _add "$(part_package "$_family" tools)"
        have_all pw-record pw-play || _add "$(part_package "$_family" play)"
      else
        _add "$(part_package "$_family" "$_part")"
      fi
      _stopped="yes"
    elif [ -z "$_unit" ]; then
      _state="installed"
    elif [ -n "$CHECK_BLIND" ]; then
      _state="installed (whether it runs is unknown: running as root)"; _unknown="yes"
    else
      case "$(runs "$_unit" "$_proc")" in
        yes) _state="installed, running" ;;
        no) _state="installed, not running"; _stopped="yes" ;;
        *) _state="installed (whether it runs is unknown: no user session here)"; _unknown="yes" ;;
      esac
    fi
    printf '  %-16s %s\n' "$_label" "$_state"
  done
  _server=""
  [ -n "$CHECK_BLIND" ] || _server="$(pulse_server)"
  _pulse=""
  case "$_server" in
    *PipeWire*|*pipewire*|"") ;;
    *) _pulse="yes"
       printf '  %-16s %s\n' "PulseAudio" "running: apps talk to PulseAudio, not PipeWire" ;;
  esac
  if [ -z "$_absent" ] && [ -z "$_stopped" ] && [ -z "$_pulse" ]; then
    if [ -n "$_unknown" ]; then
      echo "  PipeWire, WirePlumber and pipewire-pulse are installed."
    else
      echo "  PipeWire, WirePlumber and pipewire-pulse are installed and running."
    fi
    return 0
  fi
  echo
  echo "PipeWire is the sound system HushMic works with. HushMic needs PipeWire and"
  echo "WirePlumber running, and PipeWire's command line tools."
  echo "PulseAudio-only apps (TeamSpeak, some Electron apps) also need pipewire-pulse."
  _name="$(os_field PRETTY_NAME)"
  if [ -n "$_absent" ]; then
    echo
    case "$_family" in
      debian)
        # Ubuntu 22.04 and Mint 21 run pipewire-media-session, which
        # WirePlumber replaces; apt refuses the one beside the other.
        _ms=""
        case " $_missing " in
          *" wireplumber "*)
            if command -v dpkg-query >/dev/null 2>&1 &&
               dpkg-query -W -f='${Status}' pipewire-media-session 2>/dev/null |
                 grep -q 'install ok installed'; then
              _ms=" pipewire-media-session-"
            fi ;;
        esac
        echo "To install what is missing (${_name:-Debian/Ubuntu}, apt):"
        echo "  sudo apt install $_missing$_ms"
        if [ -n "$_ms" ]; then
          echo "  (the trailing minus removes pipewire-media-session, which WirePlumber replaces)"
        fi ;;
      fedora)
        if command -v dnf >/dev/null 2>&1 || ! command -v rpm-ostree >/dev/null 2>&1; then
          echo "To install what is missing (${_name:-Fedora}, dnf):"
          echo "  sudo dnf install $_missing"
        else
          echo "To install what is missing (${_name:-Fedora}, rpm-ostree; restart afterwards):"
          echo "  sudo rpm-ostree install $_missing"
        fi ;;
      arch)
        echo "To install what is missing (${_name:-Arch Linux}, pacman):"
        echo "  sudo pacman -S --needed $_missing" ;;
      suse)
        echo "To install what is missing (${_name:-openSUSE}, zypper):"
        echo "  sudo zypper install $_missing" ;;
      *)
        echo "Install these with your distribution's package manager: $_absent." ;;
    esac
  fi
  if [ -n "$_stopped" ] || [ -n "$_pulse" ]; then
    echo
    echo "To run them in your session (as yourself, not with sudo):"
    if [ -n "$_pulse" ]; then
      echo "  systemctl --user disable --now pulseaudio.service pulseaudio.socket"
    fi
    echo "  systemctl --user enable --now pipewire.socket pipewire-pulse.socket wireplumber.service"
    echo "Then log out and back in."
  fi
  return 1
}

# After a check that found something missing: go on only when the user says
# so. Without a terminal to ask (a script, CI), or as root where the session
# can't be checked, say so and go on. HUSHMIC_TTY: where the answer is read
# from (tests).
ask_continue() {
  _tty="${HUSHMIC_TTY:-/dev/tty}"
  echo
  if [ -n "$CHECK_BLIND" ]; then
    echo "Installing HushMic anyway: as root, the check can't tell what your session runs."
    return 0
  fi
  if ( : <"$_tty" ) 2>/dev/null; then
    printf 'This script installs no packages and changes no services. Install HushMic anyway? [y/N] '
    _ans=""
    read -r _ans <"$_tty" || _ans=""
    case "$_ans" in
      [yY]|[yY][eE][sS]) return 0 ;;
    esac
    echo "Nothing was installed."
    exit 1
  fi
  echo "No terminal to ask; installing HushMic anyway (--no-check skips the check)."
}

if [ "$ACTION" = "check" ]; then
  sound_check && exit 0
  exit 1
fi

# "Start HushMic when you log in" writes a per-user autostart entry; left
# behind, every login would relaunch a binary that no longer exists. Under
# sudo, resolve the INVOKING user's home (root's $HOME is not theirs).
remove_autostart_entry() {
  if [ -n "${SUDO_USER:-}" ]; then
    _as_home="$(getent passwd "$SUDO_USER" 2>/dev/null | cut -d: -f6)"
    _as_conf="${_as_home:+$_as_home/.config}"
  else
    _as_conf="${XDG_CONFIG_HOME:-${HOME:-}/.config}"
  fi
  [ -n "$_as_conf" ] && rm -f "$_as_conf/autostart/hushmic.desktop" 2>/dev/null
  # Same for the systemd user unit: stop and disable it for the invoking
  # user (best effort — no user manager in containers or plain ssh
  # sessions), then drop the enable symlink so a later reinstall does not
  # start a binary that is not there yet. The unit file itself goes with
  # the prefix below.
  if [ -n "${SUDO_USER:-}" ]; then
    _uid="$(id -u "$SUDO_USER" 2>/dev/null)"
    [ -n "$_uid" ] && sudo -u "$SUDO_USER" XDG_RUNTIME_DIR="/run/user/$_uid" \
      systemctl --user disable --now hushmic.service >/dev/null 2>&1
  else
    systemctl --user disable --now hushmic.service >/dev/null 2>&1
  fi
  [ -n "$_as_conf" ] && rm -f "$_as_conf/systemd/user/default.target.wants/hushmic.service" 2>/dev/null
  [ -n "$_as_conf" ] && remove_generated_unit "$_as_conf/systemd/user/hushmic.service"
  return 0
}

do_uninstall() {
  echo "Removing hushmic from prefix: $PREFIX"
  user_cleanup "$DEST_BIN/hushmic"
  as_root rm -f "$DEST_BIN/hushmic" "$DEST_BIN/hushmic-uninstall"
  as_root rm -f "$DEST_LADSPA/libdpdfnet_ladspa.so"
  as_root rm -rf "$DEST_LIB"
  as_root rm -rf "$PREFIX/share/hushmic"
  as_root rm -f "$DEST_APPS/hushmic.desktop" "$DEST_APPS/hushmic-clean.desktop"
  as_root rm -f "$DEST_UNITS/hushmic.service"
  for _s in $APP_ICON_SIZES; do
    as_root rm -f "$DEST_HICOLOR/$_s/apps/hushmic.png"
  done
  as_root rm -f "$DEST_HICOLOR/scalable/apps/hushmic.svg"
  for _s in $TRAY_SIZES; do
    for _n in $TRAY_NAMES; do
      as_root rm -f "$DEST_HICOLOR/$_s/status/$_n.png"
    done
  done
  for _s in $TRAY_SYMBOLIC_DIRS; do
    for _n in $TRAY_NAMES; do
      as_root rm -f "$DEST_HICOLOR/$_s/status/hushmic-mono${_n#hushmic-tray}-symbolic.svg"
    done
  done
  prune_icon_dirs
  refresh_icon_cache
  as_root rm -rf "$DEST_LICENSES"
  remove_autostart_entry
  if [ -n "$NEED_SUDO" ]; then drop_empty_dirs sudo; else drop_empty_dirs ""; fi
  echo "Done."
}

if [ "$ACTION" = "uninstall" ]; then
  do_uninstall
  exit 0
fi

# ---------------------------------------------------------------------------
# Read-only prefix preflight
# ---------------------------------------------------------------------------
# On immutable distros (ostree-based Bazzite/Silverblue/Kinoite, A/B-image
# SteamOS) /usr is read-only even for root: without this check the install
# dies mid-payload with a raw EROFS from cp. Detect it before downloading
# anything and point at the options that actually work there.
_probe_dir="$PREFIX"
while [ ! -d "$_probe_dir" ]; do _probe_dir=$(dirname "$_probe_dir"); done
_ro_prefix=""
if command -v findmnt >/dev/null 2>&1; then
  case ",$(findmnt -no OPTIONS --target "$_probe_dir" 2>/dev/null)," in
    *,ro,*) _ro_prefix="yes" ;;
  esac
else
  # No findmnt: probe with a scratch dir at the privilege the install would
  # use, and diagnose read-only ONLY on an actual EROFS message — a sudo
  # auth failure or EACCES must not masquerade as an immutable distro (it
  # will surface with its own error at the first real install step instead).
  _probe="$_probe_dir/.hushmic-rw-probe.$$"
  trap '[ -n "${_probe:-}" ] && as_root rmdir "$_probe" 2>/dev/null || true' INT TERM EXIT
  if _probe_err="$(LC_ALL=C as_root mkdir "$_probe" 2>&1)"; then
    as_root rmdir "$_probe" 2>/dev/null || true
  else
    case "$_probe_err" in
      *"Read-only file system"*) _ro_prefix="yes" ;;
    esac
  fi
  trap - INT TERM EXIT
  _probe=""
fi
if [ -n "$_ro_prefix" ]; then
  cat >&2 <<EOF
error: '$PREFIX' is on a read-only filesystem.

This looks like an immutable distro (Bazzite, Silverblue, Kinoite,
SteamOS, ...), where even root cannot write to /usr. Options that work:

  AppImage — nothing to install at all:
      https://github.com/Fovty/hushmic/releases/latest

  User-local install (no sudo needed):
      curl -fsSL https://raw.githubusercontent.com/Fovty/hushmic/main/scripts/install.sh | sh -s -- --prefix "\$HOME/.local"

  /usr/local — writable on ostree-based systems (Bazzite, Silverblue,
  Kinoite — not SteamOS, whose /usr/local is read-only too):
      curl -fsSL https://raw.githubusercontent.com/Fovty/hushmic/main/scripts/install.sh | sudo sh -s -- --prefix /usr/local
EOF
  exit 1
fi

# The sound system, before anything is downloaded or installed.
if [ -z "$NO_CHECK" ]; then
  sound_check || ask_continue
  echo
fi

# ---------------------------------------------------------------------------
# Locate the payload (co-located or downloaded)
# ---------------------------------------------------------------------------
SCRIPT_DIR="$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)"
CLEANUP_DIR=""
GEN_DIR=""
cleanup() {
  [ -n "$CLEANUP_DIR" ] && rm -rf "$CLEANUP_DIR"
  [ -n "$GEN_DIR" ] && rm -rf "$GEN_DIR"
  return 0
}
trap cleanup EXIT INT TERM

# Co-located mode needs $0 to be a REAL script file sitting next to a release
# payload: under `curl | sh` $0 is just "sh" and SCRIPT_DIR resolves to the
# CALLER'S CWD, which must never be mistaken for a payload (e.g. running the
# one-liner from inside an old extracted release would silently install that
# stale version instead of downloading the latest).
if [ -f "$0" ] && [ -f "$SCRIPT_DIR/bin/hushmic" ] \
   && [ -f "$SCRIPT_DIR/lib/ladspa/libdpdfnet_ladspa.so" ]; then
  PAYLOAD="$SCRIPT_DIR"
  echo "Installing from co-located release payload: $PAYLOAD"
else
  echo "No co-located payload; downloading the latest release..."
  if ! command -v curl >/dev/null 2>&1 && ! command -v wget >/dev/null 2>&1; then
    echo "error: need curl or wget to download the release." >&2
    exit 1
  fi
  fetch() { if command -v curl >/dev/null 2>&1; then curl -fsSL "$1"; else wget -qO- "$1"; fi; }
  # The checksums file has a stable name under releases/latest/, so it both
  # locates the current tarball (no GitHub API — no rate limits, no JSON
  # scraping) and lets us VERIFY the download before installing it as root.
  SUMS_URL="https://github.com/${REPO}/releases/latest/download/sha256sums.txt"
  if ! SUMS="$(fetch "$SUMS_URL")"; then
    echo "error: could not download $SUMS_URL" >&2
    echo "       (network problem, or no published release?)" >&2
    exit 1
  fi
  ENTRY="$(printf '%s\n' "$SUMS" | grep -E "hushmic-[0-9][^ ]*-${ARCH}\.tar\.gz\$" | head -1)"
  if [ -z "$ENTRY" ]; then
    echo "error: no hushmic-*-${ARCH}.tar.gz entry in the release's sha256sums.txt." >&2
    exit 1
  fi
  TGZ_SHA="${ENTRY%% *}"
  TGZ_NAME="$(basename "${ENTRY##* }")"
  CLEANUP_DIR="$(mktemp -d)"
  tgz="$CLEANUP_DIR/$TGZ_NAME"
  # Guarded explicitly: wget -q would otherwise die with NO diagnostic under
  # set -e (e.g. a 404 when a new release is published between the sums fetch
  # and this one, flipping what 'latest' points at).
  dl_url="https://github.com/${REPO}/releases/latest/download/${TGZ_NAME}"
  dl_ok=1
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL "$dl_url" -o "$tgz" || dl_ok=0
  else
    wget -qO "$tgz" "$dl_url" || dl_ok=0
  fi
  if [ "$dl_ok" != 1 ]; then
    echo "error: could not download $TGZ_NAME from $dl_url" >&2
    echo "       (If a release was published seconds ago, 'latest' may have just changed — retry.)" >&2
    exit 1
  fi
  if ! printf '%s  %s\n' "$TGZ_SHA" "$tgz" | sha256sum -c - >/dev/null 2>&1; then
    echo "error: checksum mismatch for $TGZ_NAME — corrupted or tampered download; aborting." >&2
    echo "       (If a release was published seconds ago, just retry.)" >&2
    exit 1
  fi
  echo "Checksum verified."
  tar -xzf "$tgz" -C "$CLEANUP_DIR"
  # The tarball has a single top-level dir: hushmic-<ver>-<arch>/
  PAYLOAD="$(find "$CLEANUP_DIR" -maxdepth 4 -type f -name hushmic -path '*/bin/hushmic' -exec dirname {} \; | head -1)"
  PAYLOAD="${PAYLOAD%/bin}"
  if [ -z "$PAYLOAD" ] || [ ! -f "$PAYLOAD/bin/hushmic" ]; then
    echo "error: could not locate bin/hushmic inside the downloaded tarball." >&2
    exit 1
  fi
  echo "Installing from downloaded payload: $PAYLOAD"
fi

# Version for the summary message, parsed from the payload dir name
# (hushmic-<ver>-<arch>); falls back gracefully if the name is unexpected.
VERSION="$(basename "$PAYLOAD" | sed -n "s/^hushmic-\(.*\)-${ARCH}\$/\1/p")"
[ -n "$VERSION" ] || VERSION="(unknown)"

# ---------------------------------------------------------------------------
# Install
# ---------------------------------------------------------------------------
ensure_dir() {
  # 0755 regardless of umask; 'install -d' (unlike 'mkdir -p -m') also gives
  # any parents it creates a umask-independent 0755. Dirs that already exist
  # are left untouched — we don't rewrite modes on system dirs we don't own.
  [ -d "$1" ] || as_root install -d -m 0755 "$1"
}

install_file() {
  # install_file <src> <dest-dir> <mode>
  src="$1"; destdir="$2"; mode="$3"
  ensure_dir "$destdir"
  as_root cp -f "$src" "$destdir/"
  as_root chmod "$mode" "$destdir/$(basename "$src")"
}

echo "Installing to prefix: $PREFIX"
install_file "$PAYLOAD/bin/hushmic" "$DEST_BIN" 755
install_file "$PAYLOAD/lib/ladspa/libdpdfnet_ladspa.so" "$DEST_LADSPA" 644

# ONNX Runtime shared lib(s) — preserve the symlink chain. cp gives a new
# file (source mode & ~umask), so pin the mode explicitly; chmod dereferences
# the symlinks, which pins the real file behind them too.
ensure_dir "$DEST_LIB"
as_root cp -Pf "$PAYLOAD"/lib/hushmic/libonnxruntime.so* "$DEST_LIB/"
as_root chmod 0755 "$DEST_LIB"/libonnxruntime.so*

# Models.
ensure_dir "$DEST_MODELS"
for m in "$PAYLOAD"/share/hushmic/models/*.onnx; do
  install_file "$m" "$DEST_MODELS" 644
done
# Native engine weights (a payload without them still works, on ONNX).
for m in "$PAYLOAD"/share/hushmic/models/*.weights.f32; do
  [ -f "$m" ] || continue
  install_file "$m" "$DEST_MODELS" 644
done

# Heal dirs left 0700 by the pre-v0.1.3 umask-sensitive installer: ensure_dir
# deliberately never rewrites EXISTING dirs, so upgraders would keep the broken
# modes forever. These dirs hold only hushmic's own payload (the ladspa dir's
# canonical mode is 0755 everywhere), so re-asserting 0755 is safe — it can
# only loosen, never break, and it un-breaks upgraded installs.
for d in "$DEST_LADSPA" "$DEST_LIB" "$PREFIX/share/hushmic" "$DEST_MODELS" "$DEST_LICENSES"; do
  if [ -d "$d" ]; then
    as_root chmod 0755 "$d"
  fi
done

install_file "$PAYLOAD/share/applications/hushmic.desktop" "$DEST_APPS" 644
install_file "$PAYLOAD/share/applications/hushmic-clean.desktop" "$DEST_APPS" 644
# systemd only reads user units from /usr/lib and /usr/local/lib (plus
# ~/.config); other prefixes generate one with `hushmic service install`.
case "$PREFIX" in
  /usr|/usr/local)
    if [ -f "$PAYLOAD/lib/systemd/user/hushmic.service" ]; then
      # The packaged unit names /usr/bin/hushmic; point it at this prefix.
      _unit_tmp="$(mktemp -d)/hushmic.service"
      sed "s|^ExecStart=/usr/bin/hushmic |ExecStart=$PREFIX/bin/hushmic |" \
        "$PAYLOAD/lib/systemd/user/hushmic.service" > "$_unit_tmp"
      install_file "$_unit_tmp" "$DEST_UNITS" 644
      rm -rf "$(dirname "$_unit_tmp")"
    fi ;;
esac
# App icon (guarded per file: payloads before v1.0 carry only the 256 px one).
for _s in $APP_ICON_SIZES; do
  _icon="$PAYLOAD/share/icons/hicolor/$_s/apps/hushmic.png"
  if [ -f "$_icon" ]; then
    install_file "$_icon" "$DEST_HICOLOR/$_s/apps" 644
  fi
done
_icon="$PAYLOAD/share/icons/hicolor/scalable/apps/hushmic.svg"
if [ -f "$_icon" ]; then
  install_file "$_icon" "$DEST_HICOLOR/scalable/apps" 644
fi

# Tray status icons (guarded per file: pre-tray payloads simply lack them).
for _s in $TRAY_SIZES; do
  for _n in $TRAY_NAMES; do
    _icon="$PAYLOAD/share/icons/hicolor/$_s/status/$_n.png"
    if [ -f "$_icon" ]; then
      install_file "$_icon" "$DEST_HICOLOR/$_s/status" 644
      # 0.9.0-rc.2 shipped this set as hushmic-tray*-symbolic; Plasma picks a
      # -symbolic twin over the colored name, so a leftover breaks tray_icon=color.
      as_root rm -f "$DEST_HICOLOR/$_s/status/$_n-symbolic.svg"
    fi
  done
done
# Same for the monochrome set (payloads older than it simply lack the files).
for _s in $TRAY_SYMBOLIC_DIRS; do
  for _n in $TRAY_NAMES; do
    _icon="$PAYLOAD/share/icons/hicolor/$_s/status/hushmic-mono${_n#hushmic-tray}-symbolic.svg"
    if [ -f "$_icon" ]; then
      install_file "$_icon" "$DEST_HICOLOR/$_s/status" 644
      # 0.9.0-rc.2 shipped this set as hushmic-tray*-symbolic; Plasma picks a
      # -symbolic twin over the colored name, so a leftover breaks tray_icon=color.
      as_root rm -f "$DEST_HICOLOR/$_s/status/$_n-symbolic.svg"
    fi
  done
done
refresh_icon_cache
[ -f "$PAYLOAD/LICENSE-MIT" ] && install_file "$PAYLOAD/LICENSE-MIT" "$DEST_LICENSES" 644
[ -f "$PAYLOAD/LICENSE-APACHE" ] && install_file "$PAYLOAD/LICENSE-APACHE" "$DEST_LICENSES" 644

# ---------------------------------------------------------------------------
# Generate the uninstaller (bakes this prefix).
# ---------------------------------------------------------------------------
GEN_DIR="$(mktemp -d)"
uninstaller="$GEN_DIR/hushmic-uninstall"
# PREFIX is baked in single-quoted with embedded quotes escaped, so a prefix
# containing spaces, `"`, `$` or backticks neither breaks the generated script
# nor gets expanded by it; everything else is a fully-quoted heredoc.
prefix_sq="$(printf %s "$PREFIX" | sed "s/'/'\\\\''/g")"
{
  printf '%s\n' '#!/bin/sh' \
    '# Auto-generated by hushmic install.sh — removes the install at the prefix below.' \
    'set -eu' \
    "PREFIX='${prefix_sq}'" \
    "APP_ICON_SIZES='${APP_ICON_SIZES}'" \
    "TRAY_SIZES='${TRAY_SIZES}'" \
    "TRAY_NAMES='${TRAY_NAMES}'" \
    "TRAY_SYMBOLIC_DIRS='${TRAY_SYMBOLIC_DIRS}'"
  printf '%s\n' "$USER_CLEANUP"
  printf '%s\n' "$PREFIX_DIRS"
  cat <<'UNINSTALL_EOF'
user_cleanup "$PREFIX/bin/hushmic"
SUDO=""
if [ "$(id -u)" -ne 0 ] && _locked="$(first_unwritable)"; then
  if command -v sudo >/dev/null 2>&1; then
    SUDO="sudo"
    echo "using sudo for $PREFIX ($_locked is not writable)"
  fi
fi
$SUDO rm -f "$PREFIX/bin/hushmic" "$PREFIX/bin/hushmic-uninstall"
$SUDO rm -f "$PREFIX/lib/ladspa/libdpdfnet_ladspa.so"
$SUDO rm -rf "$PREFIX/lib/hushmic"
$SUDO rm -rf "$PREFIX/share/hushmic"
$SUDO rm -f "$PREFIX/share/applications/hushmic.desktop" "$PREFIX/share/applications/hushmic-clean.desktop"
$SUDO rm -f "$PREFIX/lib/systemd/user/hushmic.service"
for _s in $APP_ICON_SIZES; do
  $SUDO rm -f "$PREFIX/share/icons/hicolor/$_s/apps/hushmic.png"
done
$SUDO rm -f "$PREFIX/share/icons/hicolor/scalable/apps/hushmic.svg"
for _s in $TRAY_SIZES; do
  for _n in $TRAY_NAMES; do
    $SUDO rm -f "$PREFIX/share/icons/hicolor/$_s/status/$_n.png"
  done
done
for _s in $TRAY_SYMBOLIC_DIRS; do
  for _n in $TRAY_NAMES; do
    $SUDO rm -f "$PREFIX/share/icons/hicolor/$_s/status/hushmic-mono${_n#hushmic-tray}-symbolic.svg"
  done
done
# Drop the icon directories that are now empty; never under /usr, where
# hicolor-icon-theme owns them.
case "$PREFIX" in
  /usr|/usr/) ;;
  *)
    for _d in "$PREFIX/share/icons/hicolor"/*/apps "$PREFIX/share/icons/hicolor"/*/status \
              "$PREFIX/share/icons/hicolor"/* "$PREFIX/share/icons/hicolor"; do
      if [ -d "$_d" ]; then $SUDO rmdir "$_d" 2>/dev/null || true; fi
    done ;;
esac
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
  $SUDO gtk-update-icon-cache -f "$PREFIX/share/icons/hicolor" >/dev/null 2>&1 || true
fi
$SUDO rm -rf "$PREFIX/share/licenses/hushmic"
# Also drop the per-user autostart entry (would relaunch a removed binary).
if [ -n "${SUDO_USER:-}" ]; then
  _h="$(getent passwd "$SUDO_USER" 2>/dev/null | cut -d: -f6)"
  _c="${_h:+$_h/.config}"
else
  _c="${XDG_CONFIG_HOME:-${HOME:-}/.config}"
fi
[ -n "$_c" ] && rm -f "$_c/autostart/hushmic.desktop" 2>/dev/null || true
# Stop the systemd user unit if it runs (best effort), then drop its enable symlink.
if [ -n "${SUDO_USER:-}" ]; then
  _u="$(id -u "$SUDO_USER" 2>/dev/null)"
  [ -n "$_u" ] && sudo -u "$SUDO_USER" XDG_RUNTIME_DIR="/run/user/$_u" systemctl --user disable --now hushmic.service >/dev/null 2>&1 || true
else
  systemctl --user disable --now hushmic.service >/dev/null 2>&1 || true
fi
[ -n "$_c" ] && rm -f "$_c/systemd/user/default.target.wants/hushmic.service" 2>/dev/null || true
[ -n "$_c" ] && remove_generated_unit "$_c/systemd/user/hushmic.service"
drop_empty_dirs "$SUDO"
echo "hushmic uninstalled from $PREFIX."
UNINSTALL_EOF
} > "$uninstaller"
chmod +x "$uninstaller"
install_file "$uninstaller" "$DEST_BIN" 755

# ---------------------------------------------------------------------------
# Next steps
# ---------------------------------------------------------------------------
echo
echo "hushmic ${VERSION} installed."
echo
echo "Start it:                hushmic          (tray + window; --tray = tray only)"
if [ -f "$0" ]; then
  echo "Uninstall:               hushmic-uninstall   (or: $0 --uninstall --prefix \"$PREFIX\")"
else
  # piped via `curl | sh`: $0 is just "sh", useless as a re-run hint
  echo "Uninstall:               hushmic-uninstall"
fi
# Not a package, so nothing pulls it in: ffmpeg is what Save writes most
# formats with (libopus is linked into the binary).
if ! command -v ffmpeg >/dev/null 2>&1; then
  echo
  echo "NOTE: without ffmpeg, cleaned recordings save as FLAC (WAV stays WAV) and"
  echo "      videos as their sound alone; install ffmpeg to keep the original format."
fi
# The binary finds its plugin and models from its own location (any
# prefix); --version prints where it looks. Only when that is not where
# they were put (an unusual layout) do the variables help.
_found=yes
_paths="$("$DEST_BIN/hushmic" --version 2>/dev/null)" || _found=no
for _what in plugin models; do
  _p="$(printf '%s\n' "$_paths" | sed -n "s/^$_what: //p")"
  if [ -z "$_p" ] || [ ! -e "$_p" ]; then _found=no; fi
done
if [ "$_found" = no ]; then
  echo
  echo "NOTE: hushmic does not find the files it was installed with; export these:"
  echo "  export ORT_DYLIB_PATH=\"$DEST_LIB/libonnxruntime.so\""
  echo "  export HUSHMIC_MODEL_DIR=\"$DEST_MODELS\""
  echo "  export HUSHMIC_PLUGIN_SO=\"$DEST_LADSPA/libdpdfnet_ladspa.so\""
fi
case ":${PATH:-}:" in
  *":$DEST_BIN:"*) : ;;
  *)
    echo
    echo "NOTE: $DEST_BIN is not on your PATH; add it to run hushmic by name:"
    echo "  export PATH=\"$DEST_BIN:\$PATH\""
    ;;
esac
# Where systemd reads no user units, the unit is generated for this path.
case "$PREFIX" in
  /usr|/usr/local) : ;;
  *)
    echo
    echo "To run it without a window at login (systemd user unit): hushmic service install"
    ;;
esac
