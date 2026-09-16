#!/usr/bin/env bash
# Runs on the laptop: launch an EC2 instance with local NVMe, ship the repo, restore the cached
# datadir from S3, run scripts/aws/bench.sh, fetch results, terminate.
#
#   scripts/aws/run.sh --bucket NAME [--key reth-mainnet.tar.zst] [--type i4i.4xlarge] [--region us-east-2] [--spot] [--no-wait]
#
# Environment: BASELINE=0 skips reth's rebuild, SERIAL=0 skips the one-thread control run;
# MAX_HOURS (default 12) caps the instance lifetime. With --no-wait the script returns once the
# benchmark is handed to the box and prints the fetch command to run later.
#
# Needs AWS credentials locally (aws sts get-caller-identity must work) with EC2 and S3 rights.
# The S3 credentials are handed to the instance's shell for the restore only, never written there.
set -euo pipefail

TYPE=i4i.4xlarge; REGION="${AWS_REGION:-us-east-2}"; BUCKET="${BUCKET:-}"; KEY=reth-mainnet.tar.zst
SPOT=0; WAIT=1
# The security group and instance role are named after NAME and created on first use. The key
# pair is imported from SSH_KEY_FILE under SSH_KEY_NAME on first use; override both for your own key.
NAME="${NAME:-reth-trie-rebuild-bench}"
SSH_KEY_NAME="${SSH_KEY_NAME:-hetzner-bench}"
SSH_KEY_FILE="${SSH_KEY_FILE:-$HOME/.ssh/id_ed25519_hetzner_bench}"
while [ $# -gt 0 ]; do
  case "$1" in
    --type) TYPE=$2; shift 2;;
    --region) REGION=$2; shift 2;;
    --bucket) BUCKET=$2; shift 2;;
    --key) KEY=$2; shift 2;;
    --name) NAME=$2; shift 2;;
    --spot) SPOT=1; shift;;
    --no-wait) WAIT=0; shift;;
    *) echo "unknown arg $1"; exit 2;;
  esac
done
[ -n "$BUCKET" ] || { echo "--bucket required: an S3 bucket holding the datadir tar under KEY"; exit 2; }
export AWS_DEFAULT_REGION="$REGION"

REPO=$(cd "$(dirname "$0")/../.." && pwd)
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

# Key pair and a security group that only opens ssh; both idempotent.
aws ec2 describe-key-pairs --key-names "$SSH_KEY_NAME" >/dev/null 2>&1 \
  || aws ec2 import-key-pair --key-name "$SSH_KEY_NAME" --public-key-material "fileb://$SSH_KEY_FILE.pub" >/dev/null
SG=$(aws ec2 describe-security-groups --filters "Name=group-name,Values=$NAME" --query 'SecurityGroups[0].GroupId' --output text)
if [ "$SG" = None ]; then
  SG=$(aws ec2 create-security-group --group-name "$NAME" --description "reth-trie-rebuild-bench ssh" --query GroupId --output text)
  aws ec2 authorize-security-group-ingress --group-id "$SG" --protocol tcp --port 22 --cidr 0.0.0.0/0 >/dev/null
fi
AMI=$(aws ssm get-parameter --name /aws/service/canonical/ubuntu/server/24.04/stable/current/amd64/hvm/ebs-gp3/ami-id --query Parameter.Value --output text)

MARKET=()
[ "$SPOT" = 1 ] && MARKET=(--instance-market-options 'MarketType=spot,SpotOptions={SpotInstanceType=one-time,InstanceInterruptionBehavior=terminate}')
# A role that can only read and write the results bucket, so the box uploads results without any
# credentials on disk. Idempotent.
ROLE="$NAME"
if ! aws iam get-role --role-name "$ROLE" >/dev/null 2>&1; then
  aws iam create-role --role-name "$ROLE" --assume-role-policy-document \
    '{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"ec2.amazonaws.com"},"Action":"sts:AssumeRole"}]}' >/dev/null
  aws iam put-role-policy --role-name "$ROLE" --policy-name bucket --policy-document \
    "{\"Version\":\"2012-10-17\",\"Statement\":[{\"Effect\":\"Allow\",\"Action\":[\"s3:ListBucket\"],\"Resource\":\"arn:aws:s3:::$BUCKET\"},{\"Effect\":\"Allow\",\"Action\":[\"s3:GetObject\",\"s3:PutObject\"],\"Resource\":\"arn:aws:s3:::$BUCKET/*\"}]}"
  aws iam create-instance-profile --instance-profile-name "$ROLE" >/dev/null
  aws iam add-role-to-instance-profile --instance-profile-name "$ROLE" --role-name "$ROLE"
  sleep 10
fi

echo "launching $TYPE ($AMI) in $REGION, spot=$SPOT"
# Terminate on shutdown, so the instance ends itself from inside once the run is over.
ID=$(aws ec2 run-instances --image-id "$AMI" --instance-type "$TYPE" --key-name "$SSH_KEY_NAME" \
  --security-group-ids "$SG" "${MARKET[@]}" --instance-initiated-shutdown-behavior terminate \
  --iam-instance-profile "Name=$ROLE" \
  --block-device-mappings 'DeviceName=/dev/sda1,Ebs={VolumeSize=40,VolumeType=gp3,DeleteOnTermination=true}' \
  --tag-specifications "ResourceType=instance,Tags=[{Key=Name,Value=$NAME}]" \
  --query 'Instances[0].InstanceId' --output text)
echo "instance $ID"
# Until the benchmark is handed off to the box, a failure here terminates the instance.
cleanup() {
  aws ec2 terminate-instances --instance-ids "$ID" >/dev/null && echo "instance $ID terminated"
  rm -rf "$WORK"
}
trap cleanup EXIT
aws ec2 wait instance-running --instance-ids "$ID"
IP=$(aws ec2 describe-instances --instance-ids "$ID" --query 'Reservations[0].Instances[0].PublicIpAddress' --output text)
echo "instance at $IP"

SSH_OPTS=(-i "$SSH_KEY_FILE" -o BatchMode=yes -o IdentitiesOnly=yes -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=10)
SSH=(ssh "${SSH_OPTS[@]}" "ubuntu@$IP")
for i in $(seq 1 60); do
  "${SSH[@]}" true 2>/dev/null && break
  [ "$i" = 60 ] && { echo "ssh to $IP never came up"; exit 1; }
  sleep 5
done
echo "ssh up"

# Ship the working tree as git sees it: tracked and untracked files, minus ignored ones (target,
# results) and the .git dir. A `git archive` of HEAD would miss uncommitted new files.
git -C "$REPO" ls-files -co --exclude-standard -z | tar -C "$REPO" --null -T - -cf "$WORK/repo.tar" --transform 's,^,reth-trie-rebuild-bench/,'
scp "${SSH_OPTS[@]}" "$WORK/repo.tar" "ubuntu@$IP:/tmp/"
# Everything on the box runs as root, under /root.
"${SSH[@]}" 'sudo bash -c "cd /root && rm -rf reth-trie-rebuild-bench && tar -xf /tmp/repo.tar"'

eval "$(aws configure export-credentials --format env)"
CREDS="AWS_ACCESS_KEY_ID=$AWS_ACCESS_KEY_ID AWS_SECRET_ACCESS_KEY=$AWS_SECRET_ACCESS_KEY AWS_SESSION_TOKEN=${AWS_SESSION_TOKEN:-} AWS_DEFAULT_REGION=$REGION"
"${SSH[@]}" "sudo env $CREDS BUCKET=$BUCKET KEY=$KEY bash /root/reth-trie-rebuild-bench/scripts/aws/restore.sh" &
RESTORE=$!
"${SSH[@]}" 'sudo bash /root/reth-trie-rebuild-bench/scripts/aws/setup.sh'
wait $RESTORE
# Hand the run to the box: a detached finalizer runs the benchmark, uploads the results to S3 and
# shuts down (= terminates). A hard shutdown after MAX_HOURS backs that up. From here on nothing
# depends on this driver or the ssh session; the laptop can go to sleep.
RUN_ID="$(date -u +%Y%m%dT%H%M%SZ)-$TYPE"
"${SSH[@]}" "sudo shutdown -h +$((${MAX_HOURS:-12} * 60))"
"${SSH[@]}" "sudo bash -c 'cat > /root/finalize.sh'" <<EOF
#!/usr/bin/env bash
cd /root
BASELINE=${BASELINE:-1} SERIAL=${SERIAL:-1} bash /root/reth-trie-rebuild-bench/scripts/aws/bench.sh > /root/bench.log 2>&1
aws s3 cp --recursive /root/results "s3://$BUCKET/results/$RUN_ID/" || true
aws s3 cp /root/bench.log "s3://$BUCKET/results/$RUN_ID/bench.log" || true
shutdown -h now
EOF
# `setsid -f` forks, so this command returns at once instead of holding the ssh session open.
"${SSH[@]}" 'sudo bash -c "chmod +x /root/finalize.sh && setsid -f /root/finalize.sh > /root/finalize.log 2>&1 < /dev/null"'
trap 'rm -rf "$WORK"' EXIT
echo "benchmark running detached on $IP (instance $ID); results will land in s3://$BUCKET/results/$RUN_ID/"
FETCH="aws s3 cp --recursive s3://$BUCKET/results/$RUN_ID/ $REPO/results/aws/$RUN_ID/ --region $REGION --only-show-errors"
if [ "$WAIT" = 0 ]; then
  echo "fetch once the instance has terminated:"
  echo "  $FETCH"
  exit 0
fi

# Poll until the instance is gone, then fetch. Safe to interrupt and rerun the fetch by hand.
while [ "$(aws ec2 describe-instances --instance-ids "$ID" --query 'Reservations[0].Instances[0].State.Name' --output text)" != terminated ]; do
  sleep 900
done
mkdir -p "$REPO/results/aws/$RUN_ID"
$FETCH
echo "results copied to $REPO/results/aws/$RUN_ID"
