#!/usr/bin/env bash
# Step 6 of the MVP scenario (docs/24, E1), on Docker: a fresh installation backs up into a
# folder and an S3-compatible store, checks that both restore, is deleted with its database,
# and comes back from the folder with `frood restore`. The export before and after must match,
# files (kept in the installation's files folder, docs/26 §2) included.
#
#   FROOD=core/target/debug/frood deploy/e2e/backup-restore.sh
#
# Needs Docker with Compose, curl and python3. Uses ports 18097 and 54397 on 127.0.0.1.
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/../.." && pwd)
FROOD=$(cd "$(dirname "${FROOD:-frood}")" 2>/dev/null && pwd)/$(basename "${FROOD:-frood}")
[ -x "$FROOD" ] || FROOD=$(command -v frood)
WORK=${WORK:-$(mktemp -d)}
NAME=e2e-backup
PORT=18097
S3=frood-$NAME-s3
API=http://127.0.0.1:$PORT

say() { printf '\n== %s\n' "$*"; }
secret() { python3 -c 'import secrets; print(secrets.token_urlsafe(18))'; }

cleanup() {
  for config in "$WORK/restored" "$WORK/src"; do
    [ -f "$config/.frood/deploy/compose.yaml" ] && docker compose -f "$config/.frood/deploy/compose.yaml" down -v >/dev/null 2>&1 || true
  done
  docker rm -f "$S3" >/dev/null 2>&1 || true
}
trap cleanup EXIT

say "a new installation with backups to a folder and to S3"
"$FROOD" init --dir "$WORK/src" --name "$NAME" --server container --build "$ROOT/core" --postgres-port 54397
"$FROOD" backup key --out "$WORK/recovery-key.txt" >/dev/null
PUB=$(awk '/public key:/ {print $4}' "$WORK/recovery-key.txt")
DISK_PASSWORD=$(secret); S3_PASSWORD=$(secret); S3_SECRET=$(secret)
cat >> "$WORK/src/.frood/secrets.env" <<EOF
BACKUP_DISK_PASSWORD=$DISK_PASSWORD
BACKUP_S3_PASSWORD=$S3_PASSWORD
S3_KEY_ID=frood-e2e
S3_SECRET=$S3_SECRET
EOF
python3 - "$WORK/src/frood.yaml" "$PORT" <<'EOF'
import sys
path, port = sys.argv[1], sys.argv[2]
text = open(path).read()
text = text.replace('listen: "0.0.0.0:8080"', f'listen: "0.0.0.0:{port}"')
text = text.replace("allow: []", "allow: [alice@example.com]", 1)
text = text.replace("  providers: []", '  providers: [{kind: dev, id: dev, label: "Local sign-in"}]', 1)
open(path, "w").write(text)
EOF
cat >> "$WORK/src/frood.yaml" <<EOF

modules:
  backup:
    build: $ROOT/modules/backup-restic
    volumes: ["./disk:/repos/disk"]

backup:
  recovery_key: $PUB
  targets:
    disk: { repository: /repos/disk, password: '\${secret:BACKUP_DISK_PASSWORD}' }
    s3:
      repository: 's3:http://$S3:8333/frood-e2e'
      password: '\${secret:BACKUP_S3_PASSWORD}'
      env: { AWS_ACCESS_KEY_ID: '\${secret:S3_KEY_ID}', AWS_SECRET_ACCESS_KEY: '\${secret:S3_SECRET}' }
  keep: { last: 3 }

jobs:
  extract: { enabled: false }
  backup: { on: [{ schedule: '0 */6 * * *', timezone: UTC }] }
  backup-verify: { on: [{ schedule: '30 4 * * 0', timezone: UTC }] }
EOF
# The backup module and the server run as their own user (10001): the folders must be writable
# for it. `frood apply` hands them over when it runs as root.
mkdir -p "$WORK/src/disk" "$WORK/src/.frood/objects" && chmod 0777 "$WORK/src/disk" "$WORK/src/.frood/objects"
"$FROOD" --config "$WORK/src/frood.yaml" apply

say "an S3-compatible store on the installation's network"
python3 -c 'import json,sys; json.dump({"identities":[{"name":"frood","credentials":[{"accessKey":"frood-e2e","secretKey":sys.argv[1]}],"actions":["Admin","Read","Write","List","Tagging"]}]}, open(sys.argv[2],"w"))' "$S3_SECRET" "$WORK/s3.json"
chmod 0644 "$WORK/s3.json"
docker run -d --name "$S3" --network "frood-${NAME}_default" -v "$WORK/s3.json:/etc/seaweed/s3.json:ro" \
  chrislusf/seaweedfs:latest server -s3 -s3.config=/etc/seaweed/s3.json -dir=/data >/dev/null
sleep 8   # its S3 gateway starts after the master and the volume; restic retries meanwhile

TOKEN=$("$FROOD" --config "$WORK/src/frood.yaml" token create --email alice@example.com --label e2e \
  --scopes knowledge:read,knowledge:capture,knowledge:propose,knowledge:decide,admin)
api() { curl -fsS -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' "$@"; }
run_job() {
  local id status
  id=$(api -X POST "$API/v1/runs" -d "{\"job\":\"$1\"}" | python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])')
  for _ in $(seq 1 300); do
    status=$(api "$API/v1/runs/$id" | python3 -c 'import json,sys; print(json.load(sys.stdin)["status"])')
    case "$status" in succeeded) return 0 ;; failed|cancelled) api "$API/v1/runs/$id"; echo; return 1 ;; esac
    sleep 1
  done
  echo "$1 did not finish"; return 1
}

say "knowledge with a history"
EVIDENCE=$(api -X POST "$API/v1/sources" -d '{"kind":"note","content":"agreed with Olya: release 2.4 on Nov 14"}' \
  | python3 -c 'import json,sys; print(json.load(sys.stdin)["source"]["evidence"][0]["id"])')
api -X POST "$API/v1/proposals" -d "{\"risk\":\"new\",\"operations\":[{\"op\":\"create\",\"assertion\":{\"kind\":\"commitment\",\"text\":\"Release 2.4 on Nov 14\",\"slots\":{\"due\":\"2026-11-14\"},\"modality\":\"agreed\",\"evidence\":[\"$EVIDENCE\"]}}]}" >/dev/null
ASSERTION=$(api "$API/v1/assertions?kind=commitment" | python3 -c 'import json,sys; a=json.load(sys.stdin)[0]; print(a["assertion_id"])')
PROPOSAL=$(api -X POST "$API/v1/proposals" -d "{\"risk\":\"change\",\"base\":[{\"assertion_id\":\"$ASSERTION\",\"version\":1}],\"operations\":[{\"op\":\"supersede\",\"assertion_id\":\"$ASSERTION\",\"assertion\":{\"kind\":\"commitment\",\"text\":\"Release 2.4 on Nov 28\",\"slots\":{\"due\":\"2026-11-28\"},\"modality\":\"reported\",\"evidence\":[\"$EVIDENCE\"]}}]}" \
  | python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])')
api -X POST "$API/v1/proposals/$PROPOSAL/verdicts" -d '{"decision":"accept"}' >/dev/null
# A photo attached to it: kept in the files folder, not in the database.
printf 'JPEG-the-release-board' > "$WORK/board.jpg"
curl -fsS -H "Authorization: Bearer $TOKEN" -F "text=the release board" -F "file=@$WORK/board.jpg;type=image/jpeg" \
  "$API/v1/assertions/$ASSERTION/attachments" >/dev/null
KEPT=$(find "$WORK/src/.frood/objects" -type f ! -name '.*' | wc -l | tr -d ' ')
[ "$KEPT" = 1 ] || { echo "the photo is not in the files folder: $KEPT files"; exit 1; }

say "backup into both targets, and a restore check of both"
run_job backup
run_job backup-verify
"$FROOD" --config "$WORK/src/frood.yaml" export "$WORK/before"

say "the installation is deleted with its database"
docker network disconnect "frood-${NAME}_default" "$S3"
docker compose -f "$WORK/src/.frood/deploy/compose.yaml" down -v

say "restore from the folder into a new place"
mkdir -p "$WORK/restored"
RESTIC_PASSWORD=$DISK_PASSWORD "$FROOD" --config "$WORK/restored/frood.yaml" restore \
  --repository "$WORK/src/disk" --identity "$WORK/recovery-key.txt" --image "frood-$NAME-module-backup"
mkdir -p "$WORK/restored/disk" && chmod 0777 "$WORK/restored/disk"
"$FROOD" --config "$WORK/restored/frood.yaml" apply
docker network connect "frood-${NAME}_default" "$S3"
"$FROOD" --config "$WORK/restored/frood.yaml" export "$WORK/after"

say "the same knowledge, and the restored installation backs up again"
diff -r -x schema-version.json "$WORK/before" "$WORK/after"
[ "$(find "$WORK/restored/.frood/objects" -type f ! -name '.*' | wc -l | tr -d ' ')" = 1 ] || { echo "the files folder did not come back"; exit 1; }
# The same but for when each export was made.
python3 -c 'import json,sys; a,b=(json.load(open(p)) for p in sys.argv[1:]); [d.pop("exported_at",None) for d in (a,b)]; assert a==b,(a,b)' \
  "$WORK/before/schema-version.json" "$WORK/after/schema-version.json"
HISTORY=$(api "$API/v1/assertions/$ASSERTION/history" | python3 -c 'import json,sys; print(len(json.load(sys.stdin)))')
[ "$HISTORY" -ge 2 ] || { echo "history lost: $HISTORY versions"; exit 1; }
run_job backup
run_job backup-verify
say "ok: backup, restore check, delete, restore, same export"
