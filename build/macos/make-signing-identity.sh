#!/usr/bin/env bash
# One-time setup: creates a self-signed code-signing identity, "freemonplay Local Signing",
# in your login keychain. build/macos/bundle.sh then signs the app with it instead of an
# ad-hoc signature, so macOS keeps Screen Recording and Accessibility permissions across
# rebuilds (they're tied to the signing certificate rather than to each build).
#
# It's only for apps you build yourself on this Mac; it doesn't make the app trusted for
# anyone else. Remove it any time in Keychain Access (search "freemonplay Local Signing").
set -euo pipefail

name="freemonplay Local Signing"
keychain="$HOME/Library/Keychains/login.keychain-db"

if security find-certificate -c "$name" "$keychain" >/dev/null 2>&1; then
    echo "\"$name\" already exists in your login keychain; nothing to do."
    exit 0
fi

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

cat > "$tmp/cert.cnf" <<CNF
[req]
distinguished_name = dn
x509_extensions = ext
prompt = no
[dn]
CN = $name
[ext]
basicConstraints = critical, CA:false
keyUsage = critical, digitalSignature
extendedKeyUsage = critical, codeSigning
CNF

# macOS's own openssl (LibreSSL) writes PKCS#12 files the keychain can import.
/usr/bin/openssl req -x509 -newkey rsa:2048 -nodes -days 3650 -sha256 \
    -config "$tmp/cert.cnf" -keyout "$tmp/key.pem" -out "$tmp/cert.pem" 2>/dev/null
pass=$(/usr/bin/openssl rand -hex 16)
/usr/bin/openssl pkcs12 -export -name "$name" -inkey "$tmp/key.pem" -in "$tmp/cert.pem" \
    -out "$tmp/identity.p12" -passout "pass:$pass"

# -T lets codesign use the key without prompting each time.
security import "$tmp/identity.p12" -k "$keychain" -P "$pass" -T /usr/bin/codesign >/dev/null
echo "Imported \"$name\" into your login keychain."

echo "Marking it trusted for code signing (macOS will ask for your password)..."
security add-trusted-cert -r trustRoot -p codeSign -k "$keychain" "$tmp/cert.pem"

echo
echo "Done. Run ./build/macos/bundle.sh --install; the app will be signed with \"$name\"."
echo "Grant Screen Recording and Accessibility to freemonplay once more; after that they stick."
