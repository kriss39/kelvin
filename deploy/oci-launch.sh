#!/bin/sh
# Keeps trying to create an Always Free Ampere A1 instance named "kelvin" until Oracle has capacity
# ("Out of host capacity" is common for A1). Uses ~/.oci/config; picks the first availability
# domain, the newest Ubuntu 24.04 aarch64 image and the subnet named kelvin-public. Each round
# tries 2 OCPU / 12 GB, then 1 OCPU / 6 GB, and waits INTERVAL seconds (default 60). Prints the
# public IP and exits once an instance runs; stops at once if a "kelvin" instance already exists.
set -u
export SUPPRESS_LABEL_WARNING=True PYTHONWARNINGS=ignore
INTERVAL=${INTERVAL:-60}
SSH_PUB=${SSH_PUB:-$HOME/.ssh/kelvin_oracle_ed25519.pub}
BOOT_GB=${BOOT_GB:-150}

T=$(sed -n 's/^tenancy=//p' ~/.oci/config)
AD=$(oci iam availability-domain list --compartment-id "$T" --query 'data[0].name' --raw-output)
IMAGE=$(oci compute image list --compartment-id "$T" --operating-system "Canonical Ubuntu" \
    --operating-system-version 24.04 --shape VM.Standard.A1.Flex --sort-by TIMECREATED --sort-order DESC \
    --limit 1 --query 'data[0].id' --raw-output 2>/dev/null)
SUBNET=$(oci network subnet list --compartment-id "$T" --display-name kelvin-public --query 'data[0].id' --raw-output)
echo "AD $AD"
echo "image $IMAGE"
echo "subnet $SUBNET"

existing() {
    oci compute instance list --compartment-id "$T" --display-name kelvin \
        --query 'data[?"lifecycle-state"!=`TERMINATED` && "lifecycle-state"!=`TERMINATING`] | [0].id' --raw-output 2>/dev/null
}

report() {
    ip=$(oci compute instance list-vnics --instance-id "$1" --query 'data[0]."public-ip"' --raw-output 2>/dev/null)
    echo "instance $1"
    echo "public IP $ip"
}

n=0
while :; do
    for cfg in '{"ocpus":2,"memoryInGBs":12}' '{"ocpus":1,"memoryInGBs":6}'; do
        id=$(existing)
        if [ -n "$id" ] && [ "$id" != "null" ]; then
            echo "$(date -u +%H:%M:%S) an instance named kelvin already exists"
            oci compute instance get --instance-id "$id" --wait-for-state RUNNING --query 'data."lifecycle-state"' --raw-output >/dev/null 2>&1
            report "$id"
            exit 0
        fi
        n=$((n + 1))
        out=$(oci compute instance launch --compartment-id "$T" --availability-domain "$AD" \
            --shape VM.Standard.A1.Flex --shape-config "$cfg" --image-id "$IMAGE" --subnet-id "$SUBNET" \
            --assign-public-ip true --display-name kelvin --boot-volume-size-in-gbs "$BOOT_GB" \
            --ssh-authorized-keys-file "$SSH_PUB" --wait-for-state RUNNING \
            --query 'data.id' --raw-output 2>&1)
        case "$out" in
        ocid1.instance.*)
            echo "$(date -u +%H:%M:%S) attempt $n $cfg: created"
            report "$(echo "$out" | grep -o 'ocid1\.instance[^ ]*' | head -1)"
            exit 0
            ;;
        *[Cc]apacity* | *TooManyRequests* | *InternalError*)
            echo "$(date -u +%H:%M:%S) attempt $n $cfg: $(echo "$out" | grep -o '"message": "[^"]*"' | head -1)"
            ;;
        *)
            echo "$(date -u +%H:%M:%S) attempt $n $cfg: unexpected error, stopping"
            echo "$out"
            exit 1
            ;;
        esac
    done
    sleep "$INTERVAL"
done
