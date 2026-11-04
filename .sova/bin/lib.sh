# Sourced by .sova/bin/setup and .sova/bin/test: the checkout's toolchains, never installed here.
# Node 20, Go and golangci-lint come from mise.toml; Rust from rustup (outside mise, like CI).

die() {
  echo "sova: $*" >&2
  exit 1
}

root=${SOVA_CHECKOUT:?SOVA_CHECKOUT is not set}
cd "$root" || die "cannot cd to $root"

# mise refuses an untrusted checkout, and must never install a missing tool on its own.
export MISE_TRUSTED_CONFIG_PATHS="$root"
export MISE_AUTO_INSTALL=0
mise=$(command -v mise || true)
for m in /opt/homebrew/bin/mise /usr/local/bin/mise "$HOME/.local/bin/mise"; do
  [ -n "$mise" ] && break
  [ -x "$m" ] && mise=$m
done
[ -n "$mise" ] || die "mise not found; install it, then run 'mise install' in the main checkout"
menv=$("$mise" env -C "$root" -s bash) || die "mise env failed in $root"
eval "$menv"

# rustup's cargo proxies: the first of these that has one goes first in PATH.
for d in "$HOME/.cargo/bin" /opt/homebrew/opt/rustup/bin /usr/local/opt/rustup/bin; do
  if [ -x "$d/cargo" ]; then
    PATH="$d:$PATH"
    break
  fi
done
export PATH

case $(node -v 2>/dev/null) in
v20.*) ;;
*) die "Node 20 is required (.nvmrc; the pinned Yarn breaks on newer Node): run 'mise install' in the main checkout" ;;
esac
command -v cargo >/dev/null 2>&1 || die "cargo not found: install Rust with rustup (brew install rustup)"

# The repo's pinned Yarn (.yarnrc.yml yarnPath), whatever yarn is on PATH; checks.mjs reads
# HOPP_YARN_JS too.
yarn_js=$(sed -n 's/^yarnPath:[[:space:]]*//p' "$root/.yarnrc.yml")
[ -n "$yarn_js" ] && [ -f "$root/$yarn_js" ] || die "no yarnPath in .yarnrc.yml, or its file is missing"
export HOPP_YARN_JS="$root/$yarn_js"
yarn() {
  node "$HOPP_YARN_JS" "$@"
}
