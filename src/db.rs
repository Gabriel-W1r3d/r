//! SQLite persistence layer: playlists, playlist tracks, playback history and settings.
#![allow(dead_code)]

use rusqlite::{params, Connection, Result};

pub struct Db {
    conn: Connection,
}

pub struct PlaylistRow {
    pub id: i64,
    pub name: String,
    pub cover: Option<String>,
    pub song_count: i64,
}

pub struct ScrobbleRow {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub date: String,
}

pub struct StatRow {
    pub name: String,
    pub count: i64,
}

impl Db {
    /// Opens (or creates) the database file in the working directory.
    pub fn open() -> Result<Self> {
        let conn = Connection::open("ohm_player.db")?;
        conn.execute_batch(
            "PRAGMA foreign_keys = ON;

             CREATE TABLE IF NOT EXISTS playlists (
                 id   INTEGER PRIMARY KEY AUTOINCREMENT,
                 nome TEXT NOT NULL,
                 caminho_capa_customizada TEXT
             );

             CREATE TABLE IF NOT EXISTS playlist_musicas (
                 playlist_id   INTEGER NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
                 caminho_mp3   TEXT NOT NULL,
                 posicao_ordem INTEGER NOT NULL
             );

             CREATE TABLE IF NOT EXISTS historico_reproducao (
                 id              INTEGER PRIMARY KEY AUTOINCREMENT,
                 titulo          TEXT NOT NULL,
                 artista         TEXT NOT NULL,
                 caminho_mp3     TEXT,
                 data_reproducao TEXT NOT NULL DEFAULT (datetime('now', 'localtime'))
             );

             CREATE TABLE IF NOT EXISTS playlist_last_accessed (
                 playlist_id  INTEGER PRIMARY KEY REFERENCES playlists(id) ON DELETE CASCADE,
                 last_played  TEXT NOT NULL DEFAULT (datetime('now', 'localtime'))
             );

             CREATE TABLE IF NOT EXISTS settings (
                 key   TEXT PRIMARY KEY,
                 value TEXT NOT NULL
             );",
        )?;
        // Migration: older databases lack the caminho_mp3 column on history.
        let has_path_col: bool = conn
            .prepare("SELECT 1 FROM pragma_table_info('historico_reproducao') WHERE name = 'caminho_mp3'")?
            .exists([])?;
        if !has_path_col {
            conn.execute(
                "ALTER TABLE historico_reproducao ADD COLUMN caminho_mp3 TEXT",
                [],
            )?;
        }
        // Migration: album column for album history and stats.
        let has_album_col: bool = conn
            .prepare("SELECT 1 FROM pragma_table_info('historico_reproducao') WHERE name = 'album'")?
            .exists([])?;
        if !has_album_col {
            conn.execute(
                "ALTER TABLE historico_reproducao ADD COLUMN album TEXT NOT NULL DEFAULT ''",
                [],
            )?;
        }
        Ok(Self { conn })
    }

    // ---------- Playlists (CRUD) ----------

    pub fn create_playlist(&self, name: &str) -> Result<i64> {
        self.conn
            .execute("INSERT INTO playlists (nome) VALUES (?1)", params![name])?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn delete_playlist(&self, id: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM playlists WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn rename_playlist(&self, id: i64, name: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE playlists SET nome = ?1 WHERE id = ?2",
            params![name, id],
        )?;
        Ok(())
    }

    pub fn set_playlist_cover(&self, id: i64, cover_path: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE playlists SET caminho_capa_customizada = ?1 WHERE id = ?2",
            params![cover_path, id],
        )?;
        Ok(())
    }

    pub fn list_playlists(&self) -> Result<Vec<PlaylistRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT p.id, p.nome, p.caminho_capa_customizada,
                    (SELECT COUNT(*) FROM playlist_musicas m WHERE m.playlist_id = p.id)
             FROM playlists p ORDER BY p.nome COLLATE NOCASE",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(PlaylistRow {
                id: r.get(0)?,
                name: r.get(1)?,
                cover: r.get(2)?,
                song_count: r.get(3)?,
            })
        })?;
        rows.collect()
    }

    // ---------- Playlist tracks ----------

    pub fn get_songs(&self, playlist_id: i64) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT caminho_mp3 FROM playlist_musicas
             WHERE playlist_id = ?1 ORDER BY posicao_ordem",
        )?;
        let rows = stmt.query_map(params![playlist_id], |r| r.get(0))?;
        rows.collect()
    }

    pub fn add_song(&self, playlist_id: i64, path: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO playlist_musicas (playlist_id, caminho_mp3, posicao_ordem)
             VALUES (?1, ?2,
                     (SELECT COALESCE(MAX(posicao_ordem), -1) + 1
                      FROM playlist_musicas WHERE playlist_id = ?1))",
            params![playlist_id, path],
        )?;
        Ok(())
    }

    pub fn remove_song(&self, playlist_id: i64, index: usize) -> Result<()> {
        let mut songs = self.get_songs(playlist_id)?;
        if index < songs.len() {
            songs.remove(index);
            self.rewrite_order(playlist_id, &songs)?;
        }
        Ok(())
    }

    /// Moves a song from one position to another and rewrites the order column.
    pub fn move_song(&self, playlist_id: i64, from: usize, to: usize) -> Result<()> {
        let mut songs = self.get_songs(playlist_id)?;
        if from < songs.len() && to < songs.len() && from != to {
            let item = songs.remove(from);
            songs.insert(to, item);
            self.rewrite_order(playlist_id, &songs)?;
        }
        Ok(())
    }

    fn rewrite_order(&self, playlist_id: i64, songs: &[String]) -> Result<()> {
        self.conn.execute(
            "DELETE FROM playlist_musicas WHERE playlist_id = ?1",
            params![playlist_id],
        )?;
        let mut stmt = self.conn.prepare(
            "INSERT INTO playlist_musicas (playlist_id, caminho_mp3, posicao_ordem)
             VALUES (?1, ?2, ?3)",
        )?;
        for (i, path) in songs.iter().enumerate() {
            stmt.execute(params![playlist_id, path, i as i64])?;
        }
        Ok(())
    }

    // ---------- Playback history (scrobbles) ----------

    pub fn add_scrobble(&self, title: &str, artist: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO historico_reproducao (titulo, artista, caminho_mp3) VALUES (?1, ?2, NULL)",
            params![title, artist],
        )?;
        Ok(())
    }

    pub fn add_scrobble_with_path(
        &self,
        title: &str,
        artist: &str,
        album: &str,
        path: &str,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO historico_reproducao (titulo, artista, album, caminho_mp3) VALUES (?1, ?2, ?3, ?4)",
            params![title, artist, album, path],
        )?;
        Ok(())
    }

    pub fn recent_scrobbles(&self, limit: i64) -> Result<Vec<ScrobbleRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT titulo, artista, album, data_reproducao
             FROM historico_reproducao ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit], |r| {
            Ok(ScrobbleRow {
                title: r.get(0)?,
                artist: r.get(1)?,
                album: r.get(2)?,
                date: r.get(3)?,
            })
        })?;
        rows.collect()
    }

    pub fn top_artists(&self, limit: i64) -> Result<Vec<StatRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT artista, COUNT(*) AS c FROM historico_reproducao
             GROUP BY artista ORDER BY c DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit], |r| {
            Ok(StatRow {
                name: r.get(0)?,
                count: r.get(1)?,
            })
        })?;
        rows.collect()
    }

    pub fn top_tracks(&self, limit: i64) -> Result<Vec<StatRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT titulo || ' — ' || artista, COUNT(*) AS c FROM historico_reproducao
             GROUP BY titulo, artista ORDER BY c DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit], |r| {
            Ok(StatRow {
                name: r.get(0)?,
                count: r.get(1)?,
            })
        })?;
        rows.collect()
    }

    pub fn top_albums(&self, limit: i64) -> Result<Vec<StatRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT album, COUNT(*) AS c FROM historico_reproducao
             WHERE album != '' GROUP BY album ORDER BY c DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit], |r| {
            Ok(StatRow {
                name: r.get(0)?,
                count: r.get(1)?,
            })
        })?;
        rows.collect()
    }

    /// Plays per day for the last `days` days (label "DD/MM", count). Oldest first.
    pub fn plays_per_day(&self, days: i64) -> Result<Vec<StatRow>> {
        let mut stmt = self.conn.prepare(
            "WITH RECURSIVE seq(n) AS (SELECT 0 UNION ALL SELECT n + 1 FROM seq WHERE n < ?1 - 1)
             SELECT strftime('%d/%m', date('now', 'localtime', '-' || (?1 - 1 - n) || ' days')),
                    (SELECT COUNT(*) FROM historico_reproducao
                     WHERE date(data_reproducao) = date('now', 'localtime', '-' || (?1 - 1 - n) || ' days'))
             FROM seq",
        )?;
        let rows = stmt.query_map(params![days], |r| {
            Ok(StatRow {
                name: r.get(0)?,
                count: r.get(1)?,
            })
        })?;
        rows.collect()
    }

    /// Distinct artists ever scrobbled.
    pub fn unique_artists(&self) -> Result<i64> {
        let mut stmt = self
            .conn
            .prepare("SELECT COUNT(DISTINCT artista) FROM historico_reproducao")?;
        stmt.query_row([], |r| r.get(0))
    }

    /// Plays registered today.
    pub fn plays_today(&self) -> Result<i64> {
        let mut stmt = self.conn.prepare(
            "SELECT COUNT(*) FROM historico_reproducao
             WHERE date(data_reproducao) = date('now', 'localtime')",
        )?;
        stmt.query_row([], |r| r.get(0))
    }

    // ---------- Recent tracking ----------

    pub fn track_playlist_access(&self, playlist_id: i64) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO playlist_last_accessed (playlist_id, last_played)
             VALUES (?1, datetime('now', 'localtime'))",
            params![playlist_id],
        )?;
        Ok(())
    }

    pub fn recent_playlists(&self, limit: i64) -> Result<Vec<(i64, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT p.id, p.nome FROM playlists p
             LEFT JOIN playlist_last_accessed pla ON p.id = pla.playlist_id
             ORDER BY pla.last_played DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })?;
        rows.collect()
    }

    pub fn recent_songs(&self, limit: i64) -> Result<Vec<(String, String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT titulo, artista, caminho_mp3 FROM historico_reproducao
             WHERE caminho_mp3 IS NOT NULL
             ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        rows.collect()
    }

    pub fn total_plays(&self) -> Result<i64> {
        let mut stmt = self.conn.prepare("SELECT COUNT(*) FROM historico_reproducao")?;
        stmt.query_row([], |r| r.get(0))
    }

    // ---------- Library index ----------

    /// Distinct track paths across every playlist (the on-device library).
    /// sort: 0 = caller sorts (A-Z by metadata), 1 = recently added, 2 = recently played.
    pub fn library_paths(&self, sort: i32) -> Result<Vec<String>> {
        let sql = match sort {
            1 => {
                "SELECT caminho_mp3 FROM playlist_musicas
                 GROUP BY caminho_mp3 ORDER BY MIN(rowid) DESC"
            }
            2 => {
                "SELECT m.caminho_mp3 FROM playlist_musicas m
                 LEFT JOIN (SELECT caminho_mp3, MAX(id) AS last FROM historico_reproducao
                            WHERE caminho_mp3 IS NOT NULL GROUP BY caminho_mp3) h
                     ON h.caminho_mp3 = m.caminho_mp3
                 GROUP BY m.caminho_mp3
                 ORDER BY h.last IS NULL, h.last DESC"
            }
            _ => {
                "SELECT caminho_mp3 FROM playlist_musicas
                 GROUP BY caminho_mp3 ORDER BY MIN(rowid)"
            }
        };
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        rows.collect()
    }

    // ---------- Settings (key/value) ----------

    pub fn get_setting(&self, key: &str) -> Option<String> {
        self.conn
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                params![key],
                |r| r.get(0),
            )
            .ok()
    }

    pub fn set_setting(&self, key: &str, value: &str) {
        let _ = self.conn.execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
            params![key, value],
        );
    }
}
