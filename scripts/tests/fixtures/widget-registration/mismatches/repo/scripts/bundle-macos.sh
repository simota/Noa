#!/bin/sh
# Fixture: the macOS bundle.
for widget in alpha beta delta epsilon zeta theta kappa xi nu; do
  cargo build --manifest-path "$ROOT/widgets/$widget/Cargo.toml" --locked
done

ALPHA_BIN="$TARGET_DIR/alpha"
BETA_BIN="$TARGET_DIR/beta"
DELTA_BIN="$TARGET_DIR/delta"
EPSILON_BIN="$TARGET_DIR/epsilon"
ZETA_BIN="$TARGET_DIR/zeta"
THETA_BIN="$TARGET_DIR/theta"
KAPPA_BIN="$TARGET_DIR/kappa"
XI_BIN="$TARGET_DIR/xi"
NU_BIN="$TARGET_DIR/nu"

[ -x "$ALPHA_BIN" ] || { echo "error: binary not found" >&2; exit 1; }
[ -x "$BETA_BIN" ] || { echo "error: binary not found" >&2; exit 1; }
[ -x "$DELTA_BIN" ] || { echo "error: binary not found" >&2; exit 1; }
[ -x "$EPSILON_BIN" ] || { echo "error: binary not found" >&2; exit 1; }
[ -x "$ZETA_BIN" ] || { echo "error: binary not found" >&2; exit 1; }
[ -x "$THETA_BIN" ] || { echo "error: binary not found" >&2; exit 1; }
[ -x "$KAPPA_BIN" ] || { echo "error: binary not found" >&2; exit 1; }
[ -x "$XI_BIN" ] || { echo "error: binary not found" >&2; exit 1; }
[ -x "$NU_BIN" ] || { echo "error: binary not found" >&2; exit 1; }

cp "$ALPHA_BIN" "$CONTENTS/Helpers/Noa Alpha"
cp "$BETA_BIN" "$CONTENTS/Helpers/Noa Beta"
cp "$DELTA_BIN" "$CONTENTS/Helpers/Noa Dlta"
cp "$EPSILON_BIN" "$CONTENTS/Helpers/Noa Epsilon"
cp "$ZETA_BIN" "$CONTENTS/Helpers/Noa Zeta"
cp "$THETA_BIN" "$CONTENTS/Helpers/Noa Theta"
cp "$KAPPA_BIN" "$CONTENTS/Helpers/Noa Kappa"
cp "$XI_BIN" "$CONTENTS/Helpers/Noa Xi"
cp "$NU_BIN" "$CONTENTS/Helpers/Noa Nu"

if [ -n "$NOA_SIGN_IDENTITY" ]; then
  codesign --force --options runtime --timestamp \
    --sign "$NOA_SIGN_IDENTITY" "$CONTENTS/Helpers/Noa Alpha"
  codesign --force --options runtime --timestamp \
    --sign "$NOA_SIGN_IDENTITY" "$CONTENTS/Helpers/Noa Beta"
  codesign --force --options runtime --timestamp \
    --sign "$NOA_SIGN_IDENTITY" "$CONTENTS/Helpers/Noa Dlta"
  codesign --force --options runtime --timestamp \
    --sign "$NOA_SIGN_IDENTITY" "$CONTENTS/Helpers/Noa Epsilon"
  codesign --force --options runtime --timestamp \
    --sign "$NOA_SIGN_IDENTITY" "$CONTENTS/Helpers/Noa Theta"
  codesign --force --options runtime --timestamp \
    --sign "$NOA_SIGN_IDENTITY" "$CONTENTS/Helpers/Noa Kappa"
  codesign --force --options runtime --timestamp \
    --sign "$NOA_SIGN_IDENTITY" "$CONTENTS/Helpers/Noa Xi"
  codesign --force --options runtime --timestamp \
    --sign "$NOA_SIGN_IDENTITY" "$CONTENTS/Helpers/Noa Nu"
fi
