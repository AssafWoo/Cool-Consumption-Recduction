#!/usr/bin/env bash
# Usage: ./scripts/release.sh 0.5.22
# Bumps the version in Cargo.toml, commits, tags, and pushes.
# Formula/ccr.rb is intentionally NOT touched here — CI owns it entirely.
set -euo pipefail

if [ $# -ne 1 ]; then
  echo "Usage: $0 <version>   e.g.  $0 0.5.22"
  exit 1
fi

VERSION="$1"
TAG="v${VERSION}"

# Validate semver-ish
if ! [[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "Error: version must be X.Y.Z (got '$VERSION')"
  exit 1
fi

# Must be on main and clean
BRANCH=$(git rev-parse --abbrev-ref HEAD)
if [ "$BRANCH" != "main" ]; then
  echo "Error: must be on main (currently on '$BRANCH')"
  exit 1
fi

if ! git diff --quiet || ! git diff --cached --quiet; then
  echo "Error: working tree is not clean — commit or stash changes first"
  exit 1
fi

# Check tag doesn't already exist
if git tag | grep -q "^${TAG}$"; then
  echo "Error: tag $TAG already exists"
  exit 1
fi

echo "Releasing $TAG ..."

# Bump version in ccr/Cargo.toml only
sed -i '' "s/^version = \"[0-9.]*\"/version = \"${VERSION}\"/" ccr/Cargo.toml

# Verify the change looks right
grep "^version = " ccr/Cargo.toml

# Commit only Cargo.toml — never Formula/ccr.rb
git add ccr/Cargo.toml
git commit -m "chore: bump version to ${VERSION}"

# Pull latest before pushing (avoid rejection if main moved)
git pull --rebase origin main

git push origin main
git tag "$TAG"
git push origin "$TAG"

echo ""
echo "Done. CI is now building $TAG."
echo "Formula/ccr.rb will be updated automatically once the build completes (~5 min)."
echo "Users can upgrade with: brew update && brew upgrade assafwoo/ccr/ccr"
