//! Track unfinished sandbox creation without releasing ownership before file cleanup.

use sea_orm_migration::{
    prelude::*,
    sea_orm::{DatabaseBackend, Statement},
};

//--------------------------------------------------------------------------------------------------
// Types
//--------------------------------------------------------------------------------------------------

/// Persist create intent and the exact named volumes created by it.
pub struct Migration;

//--------------------------------------------------------------------------------------------------
// Trait Implementations
//--------------------------------------------------------------------------------------------------

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20261023_000001_sandbox_creation"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "CREATE TABLE sandbox_creation (
                sandbox_id INTEGER PRIMARY KEY REFERENCES sandbox(id) ON DELETE RESTRICT,
                owner_pid INTEGER NOT NULL,
                prepared INTEGER NOT NULL DEFAULT 0 CHECK (prepared IN (0, 1))
            );
            CREATE TABLE sandbox_creation_volume (
                volume_id INTEGER PRIMARY KEY REFERENCES volume(id) ON DELETE RESTRICT,
                sandbox_id INTEGER NOT NULL REFERENCES sandbox_creation(sandbox_id) ON DELETE CASCADE
            );
            CREATE INDEX sandbox_creation_volume_owner ON sandbox_creation_volume(sandbox_id);
            CREATE TRIGGER sandbox_creation_no_restart BEFORE UPDATE OF status ON sandbox
            WHEN NEW.status IN ('Starting', 'Running') AND OLD.status IN ('Created', 'Stopped', 'Crashed')
                 AND EXISTS (SELECT 1 FROM sandbox_creation WHERE sandbox_id = OLD.id)
            BEGIN SELECT RAISE(ABORT, 'incomplete sandbox creation; remove or replace it before starting'); END;",
        ).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        let pending = db
            .query_one_raw(Statement::from_string(
                DatabaseBackend::Sqlite,
                "SELECT sandbox_id FROM sandbox_creation LIMIT 1",
            ))
            .await?;
        if pending.is_some() {
            return Err(DbErr::Migration(
                "unfinished sandbox creation remains; remove the incomplete sandboxes before downgrading".into(),
            ));
        }
        db.execute_unprepared("DROP TRIGGER sandbox_creation_no_restart; DROP TABLE sandbox_creation_volume; DROP TABLE sandbox_creation;").await?;
        Ok(())
    }
}

//--------------------------------------------------------------------------------------------------
// Tests
//--------------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Migrator, MigratorTrait};
    use sea_orm_migration::sea_orm::Database;

    #[tokio::test]
    async fn pending_creation_blocks_restart_deletion_and_downgrade() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        db.execute_unprepared("PRAGMA foreign_keys=ON")
            .await
            .unwrap();
        Migrator::up(&db, None).await.unwrap();
        db.execute_unprepared(
            "INSERT INTO sandbox (id, name, config, status, ephemeral) VALUES (1, 'incomplete', '{}', 'Crashed', 0);
             INSERT INTO sandbox_creation (sandbox_id, owner_pid) VALUES (1, 1);
             INSERT INTO volume (id, name, kind) VALUES (1, 'new', 'directory');
             INSERT INTO sandbox_creation_volume VALUES (1, 1);",
        ).await.unwrap();

        assert!(
            db.execute_unprepared("UPDATE sandbox SET status='Running' WHERE id=1")
                .await
                .unwrap_err()
                .to_string()
                .contains("incomplete sandbox creation")
        );
        assert!(
            db.execute_unprepared("DELETE FROM sandbox WHERE id=1")
                .await
                .is_err()
        );
        assert!(
            db.execute_unprepared("DELETE FROM volume WHERE id=1")
                .await
                .is_err()
        );
        assert!(
            Migrator::down(&db, Some(1))
                .await
                .unwrap_err()
                .to_string()
                .contains("unfinished sandbox creation")
        );
        assert!(
            db.query_one_raw(Statement::from_string(
                DatabaseBackend::Sqlite,
                "SELECT * FROM sandbox_creation_volume WHERE sandbox_id=1"
            ))
            .await
            .unwrap()
            .is_some()
        );

        db.execute_unprepared("DELETE FROM sandbox_creation WHERE sandbox_id=1")
            .await
            .unwrap();
        db.execute_unprepared("UPDATE sandbox SET status='Running' WHERE id=1")
            .await
            .unwrap();
        Migrator::down(&db, Some(1)).await.unwrap();
    }
}
