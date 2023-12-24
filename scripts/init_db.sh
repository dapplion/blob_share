#!/usr/bin/env bash
set -x
set -eo pipefail

if ! [ -x "$(command -v mysql)" ]; then
  echo >&2 "Error: mysql is not installed."
  exit 1
fi

if ! [ -x "$(command -v sqlx)" ]; then
  echo >&2 "Error: sqlx is not installed."
  echo >&2 "Use:"
  echo >&2 "    cargo install --version='~0.7' sqlx-cli --no-default-features --features rustls,mysql"
  echo >&2 "to install it."
  exit 1
fi

DB_USER="${MYSQL_USER:=mysql}"
DB_PASSWORD="${MYSQL_PASSWORD:=password}"
DB_NAME="${MYSQL_DB:=blob_share}"
DB_PORT="${MYSQL_PORT:=3306}"
DB_HOST="${MYSQL_HOST:=localhost}"

# To connect and debug the DB:
# `mysql -h localhost --protocol=TCP -u"mysql" -p"password"`

# Allow to skip Docker if a dockerized MySQL database is already running
if [[ -z "${SKIP_DOCKER}" ]]
then
  # if a mysql container is running, print instructions to kill it and exit
  RUNNING_MYSQL_CONTAINER=$(docker ps --filter 'name=test_mysql' --format '{{.ID}}')
  if [[ -n $RUNNING_MYSQL_CONTAINER ]]; then
    echo "there is a mysql container already running, killing it"
    docker kill ${RUNNING_MYSQL_CONTAINER}
  fi
  # Launch mysql using Docker
  docker run \
      -e MYSQL_USER=${DB_USER} \
      -e MYSQL_PASSWORD=${DB_PASSWORD} \
      -e MYSQL_ROOT_PASSWORD=${DB_PASSWORD} \
      -e MYSQL_DATABASE=${DB_NAME} \
      -p "${DB_PORT}":3306 \
      -d \
      --name "test_mysql_$(date '+%s')" \
      mysql
      # Note: Adjust MySQL Docker settings as needed
fi

# Keep pinging MySQL until it's ready to accept commands
until mysql -h "${DB_HOST}" -P "${DB_PORT}" --protocol=TCP -u"${DB_USER}" -p"${DB_PASSWORD}" -e "SELECT 1" ${DB_NAME}; do
  >&2 echo "MySQL is still unavailable - sleeping"
  sleep 1
done

>&2 echo "MySQL is up and running on port ${DB_PORT} - running migrations now!"

export DATABASE_URL=mysql://${DB_USER}:${DB_PASSWORD}@${DB_HOST}:${DB_PORT}/${DB_NAME}
sqlx database create
sqlx migrate run

>&2 echo "MySQL has been migrated, ready to go!"

