#!/usr/bin/env bash
# Portable regression test for packaged-hardware output privacy.

set -euo pipefail

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
helper="$script_dir/hardware-validation-privacy.sh"
validator="$script_dir/validate-packaged-hardware.sh"

fail()
{
    printf 'hardware-validation privacy test failed: %s\n' "$*" >&2
    exit 1
}

# shellcheck source=hardware-validation-privacy.sh
. "$helper"

input='DeviceAuth=synthetic-secret token=test-token auth_token=another-token
callsign=TEST-DT signal=92 strength=81 channel_name="Synthetic News"
IPv4 192.0.2.44 IPv6 2001:db8::8 link-v6 fe80::1234:5678:9abc:def0 full-v6 2001:0db8:0000:0000:0000:0000:0000:0008
adjacent-v6 2001:db8::1 fe80::2 ::1 packets=42
tuner 105A1232 DeviceId(274338354) device_id=105A1232
587000000 Hz freq=593000000 frequency=599000000
test playback::live_hardware::tests firmware=20260313 build=89ABCDEF
2026-09-04T15:42:05.075786Z INFO GStreamer 1.28.6 model=HDHR5-4K tuners=4 channels=72
live ATSC 1.0: selected responsive favorite channel 5.1
live ATSC 1.0 evidence: handoff 450us, first video frame 900ms, stable decode 910ms after PLAYING
live ATSC 1.0 evidence: audio caps audio/x-raw, rate=(int)48000, channels=(int)2
live ATSC 1.0 evidence: factories appsrc,typefind,tsdemux,avdec_mpeg2video,osxaudiosink'
output=$(printf '%s\n' "$input" | balun_sanitize_hardware_validation_stream)

for private_value in synthetic-secret test-token another-token TEST-DT \
    'Synthetic News' 192.0.2.44 2001:db8::8 \
    fe80::1234:5678:9abc:def0 1234:5678:9abc:def0 \
    2001:0db8:0000:0000:0000:0000:0000:0008 \
    105A1232 274338354 587000000 593000000 599000000; do
    case "$output" in
        *"$private_value"*)
            fail "sanitized output retained synthetic private value: $private_value"
            ;;
    esac
done

for placeholder in '[AUTH_TOKEN]' '[CHANNEL_NAME]' '[IP_ADDRESS]' '[TUNER_ID]' \
    '[RF_FREQUENCY]'; do
    case "$output" in
        *"$placeholder"*) ;;
        *) fail "sanitized output omitted placeholder: $placeholder" ;;
    esac
done

for useful_line in \
    'callsign=[CHANNEL_NAME] signal=92 strength=81 channel_name="[CHANNEL_NAME]"' \
    'IPv4 [IP_ADDRESS] IPv6 [IP_ADDRESS] link-v6 [IP_ADDRESS] full-v6 [IP_ADDRESS]' \
    'adjacent-v6 [IP_ADDRESS] [IP_ADDRESS] [IP_ADDRESS] packets=42' \
    'test playback::live_hardware::tests firmware=20260313 build=89ABCDEF' \
    '2026-09-04T15:42:05.075786Z INFO GStreamer 1.28.6 model=HDHR5-4K tuners=4 channels=72' \
    'live ATSC 1.0: selected responsive favorite channel 5.1' \
    'live ATSC 1.0 evidence: handoff 450us, first video frame 900ms, stable decode 910ms after PLAYING' \
    'live ATSC 1.0 evidence: audio caps audio/x-raw, rate=(int)48000, channels=(int)2' \
    'live ATSC 1.0 evidence: factories appsrc,typefind,tsdemux,avdec_mpeg2video,osxaudiosink'; do
    case "$output" in
        *"$useful_line"*) ;;
        *) fail "sanitizer damaged representative live output: $useful_line" ;;
    esac
done

if grep -F 'RAW_LOG' "$validator" >/dev/null; then
    fail 'validator still declares a raw log'
fi
if grep -F 'balun-hardware-test.' "$validator" >/dev/null; then
    fail 'validator still creates the former raw-log filename'
fi
grep -F '| balun_sanitize_hardware_validation_stream' "$validator" >/dev/null \
    || fail 'sanitizer is not in the live test pipeline'
grep -F '| tee "$SANITIZED_LOG"' "$validator" >/dev/null \
    || fail 'sanitized log is not written after stream filtering'
grep -F 'rm -f -- "$SANITIZED_LOG"' "$validator" >/dev/null \
    || fail 'temporary sanitized log is not covered by global cleanup'

printf 'hardware-validation privacy tests passed\n'
