-- Schema of the `feed-source-mysql` feature of z40-fundament (MariaDB >= 10.11 / MySQL >= 8).
--
-- Copy these statements into a migration of your app. The library never runs migrations in your
-- database. With a table prefix (`SourceTables::with_prefix("app_")`) put the prefix in front of every
-- table name, or print the prefixed statements with `SourceTables::schema()`.
-- All times are UTC.

-- The outbox: one row per change, written by the app in the transaction of the change
-- (`MySqlSource::write_event`). `cursor` is NULL until the cursor assigner numbers the committed row.
-- `id` is the order of origin and the event id on the wire. (`type`, `record_key`, `version`) is unique:
-- versions strictly increase per record.
CREATE TABLE IF NOT EXISTS `fundament_outbox` (
  `id`          BIGINT UNSIGNED NOT NULL AUTO_INCREMENT COMMENT 'order of origin, event id on the wire',
  `type`        VARCHAR(128) NOT NULL COMMENT 'record type URN',
  `record_key`  CHAR(36) NOT NULL COMMENT 'record key (lower-case UUID)',
  `version`     BIGINT UNSIGNED NOT NULL COMMENT 'version of the record',
  `deleted`     TINYINT(1) NOT NULL DEFAULT 0 COMMENT '1 = tombstone (data NULL)',
  `occurred_at` DATETIME(3) NOT NULL COMMENT 'time of the change',
  `data`        LONGTEXT NULL COMMENT 'state of the record as JSON, only fields consumers may see',
  `cursor`      BIGINT UNSIGNED NULL COMMENT 'position in the feed, assigned after commit',
  PRIMARY KEY (`id`),
  UNIQUE KEY `uq_fundament_outbox_cursor` (`cursor`),
  UNIQUE KEY `uq_fundament_outbox_version` (`type`, `record_key`, `version`),
  KEY `ix_fundament_outbox_type_cursor` (`type`, `cursor`),
  KEY `ix_fundament_outbox_type_time` (`type`, `occurred_at`),
  CONSTRAINT `ck_fundament_outbox_data` CHECK (`data` IS NULL OR JSON_VALID(`data`))
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin;

-- The last cursor ever assigned (one row). It lives apart from the outbox, so cursors are never reused
-- after the retention deleted rows.
CREATE TABLE IF NOT EXISTS `fundament_outbox_counter` (
  `id`   TINYINT UNSIGNED NOT NULL COMMENT 'always 1',
  `last` BIGINT UNSIGNED NOT NULL COMMENT 'last cursor ever assigned',
  PRIMARY KEY (`id`),
  CONSTRAINT `ck_fundament_outbox_counter_one` CHECK (`id` = 1)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin;

-- The single counter row; starts above any cursor an existing outbox may already hold.
INSERT IGNORE INTO `fundament_outbox_counter` (`id`, `last`)
SELECT 1, COALESCE(MAX(`cursor`), 0) FROM `fundament_outbox`;

-- The last poll per consumer (name of its feed key), for `abrufer` of the status document.
CREATE TABLE IF NOT EXISTS `fundament_poll` (
  `consumer`  VARCHAR(64) NOT NULL COMMENT 'name of the feed key',
  `last_poll` DATETIME(3) NOT NULL COMMENT 'time of the last answered poll',
  `cursor`    BIGINT UNSIGNED NOT NULL COMMENT 'nextCursor of that answer',
  PRIMARY KEY (`consumer`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin;

-- Failed feed key attempts per hour (no address, no key), for the note in the status document.
-- Rows older than two days are removed by `MySqlSource::purge`.
CREATE TABLE IF NOT EXISTS `fundament_poll_failure` (
  `hour`     DATETIME NOT NULL COMMENT 'start of the hour',
  `attempts` INT UNSIGNED NOT NULL COMMENT 'failed attempts in that hour',
  PRIMARY KEY (`hour`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin;
