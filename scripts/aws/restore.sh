#!/usr/bin/env bash
# Runs ON the EC2 instance: format and mount the local NVMe instance store, pull the datadir tar
# from S3 onto it, and link it into place as the reth mainnet datadir.
#
#   BUCKET=... KEY=... bash restore.sh
set -euo pipefail
: "${BUCKET:?}" "${KEY:?}"
MNT=/mnt/nvme
DATADIR="$HOME/.local/share/reth/mainnet"

apt-get install -y -q zstd unzip >/dev/null
if ! command -v aws >/dev/null; then
  cd /tmp && curl -sS "https://awscli.amazonaws.com/awscli-exe-linux-x86_64.zip" -o awscliv2.zip \
    && unzip -q -o awscliv2.zip && ./aws/install >/dev/null
fi

# The instance store shows up as an NVMe disk with no partitions and no mount, distinct from the
# EBS root volume; pick the largest one.
DEV=$(lsblk -dnb -o NAME,SIZE,TYPE,MOUNTPOINT | awk '$3=="disk" && $4=="" {print $2, $1}' | sort -n | tail -1 | awk '{print $2}')
[ -n "$DEV" ] || { echo "no unmounted disk found"; lsblk; exit 1; }
if ! mountpoint -q "$MNT"; then
  echo "formatting /dev/$DEV as ext4 and mounting at $MNT"
  mkfs.ext4 -q -F "/dev/$DEV"
  mkdir -p "$MNT"
  mount -o noatime "/dev/$DEV" "$MNT"
fi

mkdir -p "$MNT/reth" "$(dirname "$DATADIR")"
[ -e "$DATADIR" ] || ln -s "$MNT/reth" "$DATADIR"
if [ ! -f "$DATADIR/db/mdbx.dat" ]; then
  echo "restoring s3://$BUCKET/$KEY to $MNT/reth"
  aws configure set default.s3.max_concurrent_requests 16
  aws configure set default.s3.multipart_chunksize 256MB
  time (aws s3 cp "s3://$BUCKET/$KEY" - | zstd -d -T0 | tar -xf - -C "$MNT/reth")
fi
du -sh "$DATADIR"/db "$DATADIR"/static_files
df -h "$MNT"
