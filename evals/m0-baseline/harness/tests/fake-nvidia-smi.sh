#!/usr/bin/env bash
# Stand-in for `nvidia-smi --query-compute-apps=...` in tests/selftest.sh (NVIDIA_SMI=this script).
# FAKE_GPU_BUSY=1: one process holds GPU memory; FAKE_NVSMI_FAIL=1: nvidia-smi itself fails.
if [[ ${FAKE_NVSMI_FAIL:-0} == 1 ]]; then
    echo "NVIDIA-SMI has failed because it couldn't communicate with the NVIDIA driver." >&2
    exit 9
fi
if [[ ${FAKE_GPU_BUSY:-0} == 1 ]]; then
    case "$*" in
        *--query-compute-apps=pid\ *) echo "4242" ;;
        *) echo "4242, /usr/bin/python3, 1536 MiB" ;;
    esac
fi
exit 0
