-- Schema of the `replica-mysql` feature of z40-fundament (MariaDB >= 10.11 / MySQL >= 8).
--
-- Copy these statements into a migration of your app. The library never runs migrations in your
-- database. With a table prefix (`Tables::with_prefix("app_")`) put the prefix in front of every
-- table name, or print the prefixed statements with `Tables::schema()`.
-- All times are UTC.

-- Latest state per record of a source. `data` holds only what the app's Mapping keeps (JSON),
-- tombstones keep their version with `deleted = 1` and `data` NULL.
CREATE TABLE IF NOT EXISTS `fundament_replica` (
  `source`      VARCHAR(64) NOT NULL COMMENT 'source app (name of the consumer)',
  `kind`        VARCHAR(32) NOT NULL COMMENT 'record kind from the mapping',
  `record_key`  CHAR(36) NOT NULL COMMENT 'record key (lower-case UUID)',
  `version`     BIGINT UNSIGNED NOT NULL COMMENT 'version of the stored state, only higher ones are applied',
  `deleted`     TINYINT(1) NOT NULL DEFAULT 0 COMMENT '1 = tombstone (data NULL)',
  `external_id` VARCHAR(64) NULL COMMENT 'id in the app''s own data, from the mapping',
  `data`        LONGTEXT NULL COMMENT 'kept fields as JSON',
  `updated_at`  DATETIME NOT NULL COMMENT 'when this row was last written',
  PRIMARY KEY (`source`, `kind`, `record_key`),
  KEY `ix_fundament_replica_external` (`source`, `kind`, `external_id`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

-- Position and health of each consumed feed (one row per source).
CREATE TABLE IF NOT EXISTS `fundament_cursor` (
  `source`       VARCHAR(64) NOT NULL COMMENT 'source app',
  `cursor`       BIGINT UNSIGNED NOT NULL DEFAULT 0 COMMENT 'nextCursor of the last applied page',
  `last_run`     DATETIME NULL COMMENT 'end of the last run',
  `last_success` DATETIME NULL COMMENT 'end of the last successful run',
  `last_result`  VARCHAR(16) NULL COMMENT 'ok | fehler',
  `text`         VARCHAR(200) NULL COMMENT 'fixed error text of the last run, without contents',
  PRIMARY KEY (`source`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

-- Events for the status document: changes of a source between ok and failing. Fixed texts only,
-- kept 30 days.
CREATE TABLE IF NOT EXISTS `fundament_event` (
  `id`    BIGINT NOT NULL AUTO_INCREMENT,
  `time`  DATETIME NOT NULL,
  `kind`  VARCHAR(32) NOT NULL COMMENT 'e.g. feed',
  `level` VARCHAR(16) NOT NULL COMMENT 'ok | warnung | fehler',
  `text`  VARCHAR(200) NOT NULL,
  PRIMARY KEY (`id`),
  KEY `ix_fundament_event_time` (`time`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
