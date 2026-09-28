-- Accounts, the tree's revisions and who changed what, and its documents.
-- Timestamps are UTC (sqlx sets the session time zone to +00:00).

CREATE TABLE users (
    id INT AUTO_INCREMENT PRIMARY KEY,
    username VARCHAR(64) NOT NULL UNIQUE,
    display_name VARCHAR(128) NOT NULL DEFAULT '',
    -- argon2id, in PHC string form.
    password_hash VARCHAR(255) NOT NULL,
    role ENUM('admin', 'editor', 'family', 'guest') NOT NULL DEFAULT 'guest',
    disabled BOOLEAN NOT NULL DEFAULT FALSE,
    -- Part of every session token; bumping it signs the user out everywhere.
    session_epoch INT NOT NULL DEFAULT 0,
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    last_login_at TIMESTAMP NULL
) ENGINE = InnoDB DEFAULT CHARSET = utf8mb4 COLLATE = utf8mb4_unicode_ci;

-- Every saved version of the tree, whole (gzip'd GEDCOM), so any can be restored.
CREATE TABLE revisions (
    id BIGINT AUTO_INCREMENT PRIMARY KEY,
    parent_id BIGINT NULL,
    user_id INT NULL,
    created_at TIMESTAMP(3) NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
    gedcom LONGBLOB NOT NULL,
    people_count INT NOT NULL,
    note VARCHAR(255) NOT NULL DEFAULT '',
    FOREIGN KEY (parent_id) REFERENCES revisions (id),
    FOREIGN KEY (user_id) REFERENCES users (id) ON DELETE SET NULL
) ENGINE = InnoDB DEFAULT CHARSET = utf8mb4 COLLATE = utf8mb4_unicode_ci;

-- The one tree this site serves, and its current revision.
CREATE TABLE tree (
    id TINYINT PRIMARY KEY,
    name VARCHAR(128) NOT NULL,
    head_revision_id BIGINT NULL,
    FOREIGN KEY (head_revision_id) REFERENCES revisions (id)
) ENGINE = InnoDB DEFAULT CHARSET = utf8mb4 COLLATE = utf8mb4_unicode_ci;

INSERT INTO tree (id, name) VALUES (1, 'Family tree');

-- Which records each revision changed, and who changed them.
CREATE TABLE changes (
    id BIGINT AUTO_INCREMENT PRIMARY KEY,
    revision_id BIGINT NOT NULL,
    xref VARCHAR(64) NOT NULL,
    record_tag VARCHAR(32) NOT NULL,
    action ENUM('add', 'modify', 'delete') NOT NULL,
    -- What the record was called then (a person's name), for the history list.
    label VARCHAR(255) NOT NULL DEFAULT '',
    user_id INT NULL,
    created_at TIMESTAMP(3) NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
    INDEX (xref),
    INDEX (revision_id),
    FOREIGN KEY (revision_id) REFERENCES revisions (id),
    FOREIGN KEY (user_id) REFERENCES users (id) ON DELETE SET NULL
) ENGINE = InnoDB DEFAULT CHARSET = utf8mb4 COLLATE = utf8mb4_unicode_ci;

-- Documents: the GEDCOM FILE path each one is known by, and its content,
-- stored on disk as MEDIA_DIR/<sha256>.
CREATE TABLE media (
    id INT AUTO_INCREMENT PRIMARY KEY,
    -- Case matters in paths; the API reads this column through CAST(… AS CHAR).
    path VARCHAR(512) CHARACTER SET utf8mb4 COLLATE utf8mb4_bin NOT NULL,
    sha256 CHAR(64) NOT NULL,
    size BIGINT NOT NULL,
    uploaded_by INT NULL,
    uploaded_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE KEY (path),
    INDEX (sha256),
    FOREIGN KEY (uploaded_by) REFERENCES users (id) ON DELETE SET NULL
) ENGINE = InnoDB DEFAULT CHARSET = utf8mb4 COLLATE = utf8mb4_unicode_ci;
