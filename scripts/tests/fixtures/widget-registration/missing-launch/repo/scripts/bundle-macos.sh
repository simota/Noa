#!/bin/sh
# Fixture: the macOS bundle.
for widget in alpha beta gamma; do
  cargo build --manifest-path "$ROOT/widgets/$widget/Cargo.toml" --locked
done

ALPHA_BIN="$TARGET_DIR/alpha"
BETA_BIN="$TARGET_DIR/beta"
GAMMA_BIN="$TARGET_DIR/gamma"

[ -x "$ALPHA_BIN" ] || { echo "error: binary not found" >&2; exit 1; }
[ -x "$BETA_BIN" ] || { echo "error: binary not found" >&2; exit 1; }
[ -x "$GAMMA_BIN" ] || { echo "error: binary not found" >&2; exit 1; }

cp "$ALPHA_BIN" "$CONTENTS/Helpers/Noa Alpha"
cp "$BETA_BIN" "$CONTENTS/Helpers/Noa Beta"
cp "$GAMMA_BIN" "$CONTENTS/Helpers/Noa Gamma"

if [ -n "$NOA_SIGN_IDENTITY" ]; then
  codesign --force --options runtime --timestamp \
    --sign "$NOA_SIGN_IDENTITY" "$CONTENTS/Helpers/Noa Alpha"
  codesign --force --options runtime --timestamp \
    --sign "$NOA_SIGN_IDENTITY" "$CONTENTS/Helpers/Noa Beta"
  codesign --force --options runtime --timestamp \
    --sign "$NOA_SIGN_IDENTITY" "$CONTENTS/Helpers/Noa Gamma"
fi
