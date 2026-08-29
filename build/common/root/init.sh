#!/bin/bash

set -e

# Fail if running as root (uid 0) - container must run as non-root user
if [[ $(id -u) -eq 0 ]]; then
    echo "ERROR: Container must run as non-root user (current uid=$(id -u)), use user: directive in docker-compose" >&2
    exit 1
fi

exec 3>&1 4>&2 &> >(tee -a /config/supervisord.log)

source '/usr/local/bin/system/scripts/docker/utils.sh'

cat << "EOF"
Created by...



    _____            ___  
   |___ /   _ __    ( _ ) 
     |_ \  | '_ \   / _ \ 
    ___) | | | | | | (_) |
   |____/  |_| |_|  \___/
   https://github.com/3n8

EOF

source '/etc/image-build-info'

if [[ -z "${TZ}" ]]; then
    export TZ="UTC"
fi

if [[ -f "/usr/share/zoneinfo/${TZ}" ]]; then
    ln -sf "/usr/share/zoneinfo/${TZ}" /etc/localtime 2>/dev/null || true
fi

echo "[info] Timezone set to '${TZ}'" | ts '%Y-%m-%d %H:%M:%.S'

if [[ "${HOST_OS,,}" == "unraid" ]]; then
    echo "[info] Host is running unRAID" | ts '%Y-%m-%d %H:%M:%.S'
fi

echo "[info] System information: $(uname -a)" | ts '%Y-%m-%d %H:%M:%.S'

echo "[info] Image architecture: '${TARGETARCH}'" | ts '%Y-%m-%d %H:%M:%.S'

echo "[info] Base image release tag: '${BASE_RELEASE_TAG}'" | ts '%Y-%m-%d %H:%M:%.S'

export PUID=$(id -u)
export PGID=$(id -g)

echo "[info] Running as UID='${PUID}', GID='${PGID}'" | ts '%Y-%m-%d %H:%M:%.S'

if [[ ! -f "/config/perms.txt" ]]; then
    if [[ -d "/config" ]]; then
        echo "[info] Setting ownership and permissions recursively on '/config'..." | ts '%Y-%m-%d %H:%M:%.S'
        set +e
        chown -R "${PUID}":"${PGID}" "/config" 2>/dev/null
        exit_code_chown=$?
        chmod -R 775 "/config" 2>/dev/null
        exit_code_chmod=$?
        set -e

        if (( exit_code_chown != 0 || exit_code_chmod != 0 )); then
            echo "[warn] Unable to chown/chmod '/config', assuming SMB/NFS mountpoint" | ts '%Y-%m-%d %H:%M:%.S'
        else
            echo "[info] Successfully set ownership and permissions on '/config'" | ts '%Y-%m-%d %H:%M:%.S'
        fi
    else
        echo "[info] '/config' directory does not exist, skipping" | ts '%Y-%m-%d %H:%M:%.S'
    fi

    if [[ -d "/data" ]]; then
        echo "[info] Setting ownership and permissions non-recursively on '/data'..." | ts '%Y-%m-%d %H:%M:%.S'
        set +e
        chown "${PUID}":"${PGID}" "/data" 2>/dev/null
        exit_code_chown=$?
        chmod 775 "/data" 2>/dev/null
        exit_code_chmod=$?
        set -e

        if (( exit_code_chown != 0 || exit_code_chmod != 0 )); then
            echo "[info] Unable to chown/chmod '/data', assuming SMB/NFS mountpoint" | ts '%Y-%m-%d %H:%M:%.S'
        else
            echo "[info] Successfully set ownership and permissions on '/data'" | ts '%Y-%m-%d %H:%M:%.S'
        fi
    else
        echo "[info] '/data' directory does not exist, skipping" | ts '%Y-%m-%d %H:%M:%.S'
    fi

    echo "This file prevents ownership and permissions from being applied/re-applied to '/config' and '/data'" > /config/perms.txt 2>/dev/null || true
else
    echo "[info] Permissions file '/config/perms.txt' exists, skipping" | ts '%Y-%m-%d %H:%M:%.S'
fi

disk_usage_tmp=$(du -s /tmp 2>/dev/null | awk '{print $1}' || echo "0")
if [ "${disk_usage_tmp}" -gt 1073741824 ]; then
    echo "[warn] /tmp directory contains 1GB+ of data, skipping clear down" | ts '%Y-%m-%d %H:%M:%.S'
    ls -al /tmp 2>/dev/null || true
else
    echo "[info] Deleting files in /tmp (non recursive)..." | ts '%Y-%m-%d %H:%M:%.S'
    rm -f /tmp/* > /dev/null 2>&1 || true
fi

set +e
mkdir -p /config/run 2>/dev/null
chmod 775 /config/run 2>/dev/null
set -e

DOWNLOADS="$(echo "${DOWNLOADS:-/downloads}" | sed 's/^[[:space:]]*//;s/[[:space:]]*$//;s:/*$::')"
export DOWNLOADS
mkdir -p /config/bin /config/cache /config/run "${DOWNLOADS}" 2>/dev/null || true
if [[ -d "${DOWNLOADS}" ]]; then
    echo "[info] Download directory '${DOWNLOADS}'" | ts '%Y-%m-%d %H:%M:%.S'
    set +e
    chown "${PUID}":"${PGID}" "${DOWNLOADS}" 2>/dev/null
    chmod 775 "${DOWNLOADS}" 2>/dev/null
    set -e
fi
if [[ ! -x /config/bin/yt-dlp ]]; then
    if [[ -x /usr/lib/yt-dlp-server/yt-dlp ]]; then
        echo "[info] Seeding yt-dlp binary into /config/bin" | ts '%Y-%m-%d %H:%M:%.S'
        cp /usr/lib/yt-dlp-server/yt-dlp /config/bin/yt-dlp 2>/dev/null || true
        chmod 755 /config/bin/yt-dlp 2>/dev/null || true
    fi
fi
if [[ ! -f /config/config.yml && -f /usr/lib/yt-dlp-server/default_config.yml ]]; then
    echo "[info] Installing default config.yml" | ts '%Y-%m-%d %H:%M:%.S'
    cp /usr/lib/yt-dlp-server/default_config.yml /config/config.yml 2>/dev/null || true
fi

echo "[info] Starting Supervisor..." | ts '%Y-%m-%d %H:%M:%.S'

exec 1>&3 2>&4

exec /usr/bin/supervisord -c /etc/supervisord.conf -n
