-- Schema of the `bff-mysql` feature of z40-fundament-http (MariaDB >= 10.11 / MySQL >= 8).
--
-- Copy these statements into a migration of your app. The library never runs migrations in your
-- database. With a table prefix (`SessionTables::with_prefix("app_")`) put the prefix in front of every
-- table name, or print the prefixed statements with `SessionTables::schema()`.
-- All times are UTC. No cookie value is stored: keys are SHA-256 hashes of the cookie values.

-- Sessions after a login.
CREATE TABLE IF NOT EXISTS `bff_session` (
  `id_hash`      BINARY(32) NOT NULL COMMENT 'SHA-256 of the session cookie value',
  `sub`          VARCHAR(255) CHARACTER SET utf8mb4 COLLATE utf8mb4_bin NOT NULL COMMENT 'subject at the login provider',
  `sid`          VARCHAR(255) CHARACTER SET utf8mb4 COLLATE utf8mb4_bin NULL COMMENT 'session id of the login provider (back-channel logout)',
  `data`         TEXT NOT NULL COMMENT 'session data of the app (JSON)',
  `id_token`     BLOB NULL COMMENT 'ID token sealed with a key derived from the cookie value (logout hint)',
  `created_at`   DATETIME(3) NOT NULL COMMENT 'login time',
  `last_seen_at` DATETIME(3) NOT NULL COMMENT 'last recorded use',
  `expires_at`   DATETIME(3) NOT NULL COMMENT 'end under the timeouts at the last write',
  PRIMARY KEY (`id_hash`),
  KEY `ix_bff_session_sub` (`sub`),
  KEY `ix_bff_session_sid` (`sid`),
  KEY `ix_bff_session_expires` (`expires_at`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

-- Started logins until their callback (10 minutes, single use).
CREATE TABLE IF NOT EXISTS `bff_login` (
  `id_hash`    BINARY(32) NOT NULL COMMENT 'SHA-256 of the login cookie value',
  `state`      BLOB NOT NULL COMMENT 'login state sealed with a key derived from the cookie value',
  `return_to`  VARCHAR(512) NOT NULL COMMENT 'checked return path',
  `expires_at` DATETIME(3) NOT NULL COMMENT 'end of the login',
  PRIMARY KEY (`id_hash`),
  KEY `ix_bff_login_expires` (`expires_at`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
