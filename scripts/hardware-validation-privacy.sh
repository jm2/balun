#!/usr/bin/env bash
# Shared output sanitizer for the opt-in packaged-hardware validation.
# This file is sourced by the validator and its platform-neutral policy test.

balun_sanitize_hardware_validation_stream()
{
    sed -E \
        -e 's/((DeviceAuth|device_auth|auth_token|token)"?[[:space:]]*[:=][[:space:]]*"?)[^"&,[:space:]}]+/\1[AUTH_TOKEN]/g' \
        -e 's/((GuideName|guide_name|callsign|channel_name)"?[[:space:]]*[:=][[:space:]]*")[^"]*"/\1[CHANNEL_NAME]"/g' \
        -e 's/((GuideName|guide_name|callsign|channel_name)"?[[:space:]]*[:=][[:space:]]*)[^",}[:space:]]+/\1[CHANNEL_NAME]/g' \
        -e 's/([0-9]{1,3}\.){3}[0-9]{1,3}/[IP_ADDRESS]/g' \
        -e 's/(^|[^[:alnum:]_:])([[:xdigit:]]{1,4}:){7}[[:xdigit:]]{1,4}/\1[IP_ADDRESS]/g' \
        -e 's/(^|[^[:alnum:]_:])([[:xdigit:]]{1,4}:){1,7}:([[:xdigit:]]{1,4}(:[[:xdigit:]]{1,4}){0,6})?/\1[IP_ADDRESS]/g' \
        -e 's/(^|[^[:alnum:]_:])::[[:xdigit:]]{1,4}(:[[:xdigit:]]{1,4}){0,6}/\1[IP_ADDRESS]/g' \
        -e 's/DeviceId\([0-9]{1,10}\)/DeviceId([TUNER_ID])/g' \
        -e 's/((DeviceID|device_id|TunerID|tuner_id)"?[[:space:]]*[:=][[:space:]]*"?)[[:xdigit:]]{8}([^[:xdigit:]]|$)/\1[TUNER_ID]\3/g' \
        -e 's/((tuner|Tuner)([[:space:]#:=]+))[[:xdigit:]]{8}([^[:xdigit:]]|$)/\1[TUNER_ID]\4/g' \
        -e 's/[0-9]{7,10}[[:space:]]*Hz/[RF_FREQUENCY]/g' \
        -e 's/((freq|frequency)=)[0-9]+/\1[RF_FREQUENCY]/g'
}
