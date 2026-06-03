use rusqlite::{params, Connection};
use serde::Serialize;

use crate::error::LibraryError;
use crate::library::now_unix_secs;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayHistoryItem {
    pub id: i64,
    pub video_id: String,
    pub played_at: i64,
    pub duration_played_sec: f64,
    pub position_at_close_sec: Option<f64>,
    pub title: Option<String>,
    pub thumbnail_url: Option<String>,
    pub duration_sec: Option<i64>,
    pub is_short: bool,
}

pub fn record_playback(
    conn: &Connection,
    video_id: &str,
    duration_played_sec: f64,
    position_at_close_sec: Option<f64>,
) -> Result<PlayHistoryItem, LibraryError> {
    let now = now_unix_secs();

    conn.execute(
        "INSERT INTO play_history (video_id, played_at, duration_played_sec, position_at_close_sec) \
         VALUES (?1, ?2, ?3, ?4)",
        params![video_id, now, duration_played_sec, position_at_close_sec],
    )?;

    conn.execute(
        "UPDATE videos SET play_count = play_count + 1, last_played_at = ?1 WHERE id = ?2",
        params![now, video_id],
    )?;

    let id = conn.last_insert_rowid();
    Ok(PlayHistoryItem {
        id,
        video_id: video_id.to_string(),
        played_at: now,
        duration_played_sec,
        position_at_close_sec,
        title: None,
        thumbnail_url: None,
        duration_sec: None,
        is_short: false,
    })
}

pub fn list_play_history(
    conn: &Connection,
    offset: u32,
    limit: u32,
    is_short: Option<bool>,
) -> Result<Vec<PlayHistoryItem>, LibraryError> {
    let limit = limit.min(200);
    let query = format!(
        "SELECT h.id, h.video_id, h.played_at, h.duration_played_sec, h.position_at_close_sec, \
                v.title, v.thumbnail_url, v.duration_sec, COALESCE(v.is_short, 0) \
         FROM play_history h \
         LEFT JOIN videos v ON v.id = h.video_id \
         {} \
         ORDER BY h.played_at DESC \
         LIMIT ?1 OFFSET ?2",
        if is_short.is_some() {
            "WHERE v.is_short = ?3"
        } else {
            "WHERE 1=1"
        },
    );
    let mut stmt = conn.prepare(&query)?;
    // is_short が None のときクエリは ?3 を含まない (WHERE 1=1) ので、flag を
    // バインドしてはいけない。常に 3 個渡すと None 時に「Wrong number of
    // parameters passed to query. Got 3, needed 2」で実行が落ちる。プレース
    // ホルダ数に一致させ、フィルタ指定時のみ ?3 をバインドする。
    let flag = is_short.map(|s| s as i64);
    let mut binds: Vec<&dyn rusqlite::types::ToSql> = vec![&limit, &offset];
    if let Some(ref f) = flag {
        binds.push(f);
    }
    let rows = stmt.query_map(binds.as_slice(), |row| {
        Ok(PlayHistoryItem {
            id: row.get(0)?,
            video_id: row.get(1)?,
            played_at: row.get(2)?,
            duration_played_sec: row.get(3)?,
            position_at_close_sec: row.get(4)?,
            title: row.get(5)?,
            thumbnail_url: row.get(6)?,
            duration_sec: row.get(7)?,
            is_short: row.get::<_, i64>(8)? != 0,
        })
    })?;
    let items = rows.collect::<Result<Vec<_>, _>>()?;
    Ok(items)
}

pub fn clear_play_history(conn: &Connection) -> Result<usize, LibraryError> {
    let affected = conn.execute("DELETE FROM play_history", [])?;
    Ok(affected)
}

pub fn delete_play_history_item(conn: &Connection, id: i64) -> Result<bool, LibraryError> {
    let affected = conn.execute("DELETE FROM play_history WHERE id = ?1", params![id])?;
    Ok(affected > 0)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::library::schema::run_migrations;

    fn setup() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        run_migrations(&mut conn).unwrap();
        conn
    }

    fn insert_video(conn: &Connection, id: &str, is_short: i64) {
        conn.execute(
            "INSERT INTO videos (id, title, duration_sec, is_short) VALUES (?1, ?2, ?3, ?4)",
            params![id, format!("title {id}"), 100, is_short],
        )
        .unwrap();
    }

    // Regression: is_short=None だとクエリは ?3 を含まない (WHERE 1=1) のに、
    // 以前は常に 3 個 (limit/offset/flag) をバインドしていたため
    // 「Wrong number of parameters passed to query. Got 3, needed 2」で
    // 既定の履歴一覧 (フィルタ無し) が必ず落ちていた。
    #[test]
    fn list_play_history_without_filter_does_not_error() {
        let conn = setup();
        insert_video(&conn, "sm9", 0);
        record_playback(&conn, "sm9", 42.0, Some(42.0)).unwrap();

        let all = list_play_history(&conn, 0, 50, None).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].video_id, "sm9");
        assert_eq!(all[0].duration_played_sec, 42.0);
        assert_eq!(all[0].position_at_close_sec, Some(42.0));
    }

    // フィルタ指定時 (?3 あり) も正しく動き、ショート / 非ショートを出し分ける。
    #[test]
    fn list_play_history_respects_is_short_filter() {
        let conn = setup();
        insert_video(&conn, "sm9", 0);
        insert_video(&conn, "ss1", 1);
        record_playback(&conn, "sm9", 1.0, None).unwrap();
        record_playback(&conn, "ss1", 1.0, None).unwrap();

        let shorts = list_play_history(&conn, 0, 50, Some(true)).unwrap();
        assert_eq!(shorts.len(), 1);
        assert_eq!(shorts[0].video_id, "ss1");
        assert!(shorts[0].is_short);

        let longs = list_play_history(&conn, 0, 50, Some(false)).unwrap();
        assert_eq!(longs.len(), 1);
        assert_eq!(longs[0].video_id, "sm9");
        assert!(!longs[0].is_short);

        let all = list_play_history(&conn, 0, 50, None).unwrap();
        assert_eq!(all.len(), 2);
    }

    // record_playback は videos.play_count を増やし last_played_at を更新する。
    #[test]
    fn record_playback_increments_play_count() {
        let conn = setup();
        insert_video(&conn, "sm9", 0);
        record_playback(&conn, "sm9", 5.0, None).unwrap();
        record_playback(&conn, "sm9", 7.0, None).unwrap();
        let count: i64 = conn
            .query_row("SELECT play_count FROM videos WHERE id = 'sm9'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(count, 2);
    }
}
