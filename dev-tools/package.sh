#!/bin/bash
set -eu

rm -rf releases
mkdir releases

mkdir "releases/$PKG_NAME"
cp "$1" "releases/$PKG_NAME"
cp README.md LICENSE "releases/$PKG_NAME"

pushd releases
tar -czf $PKG_NAME.tar.gz $PKG_NAME
rm -r $PKG_NAME
if [ -n "${GPG_SIGNER:-}" ]; then
  gpg -u "$GPG_SIGNER" -ab $PKG_NAME.tar.gz
fi
popd

