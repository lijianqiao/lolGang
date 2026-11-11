//! 队伍命令和个人资料：都在节点任务里执行，完了返回整份队伍页。见 `team` 文件头第 1 条。

use tauri::{AppHandle, Manager};

use super::view::{AvatarInput, TeamView, unbase64, view};
use super::{refresh_names, refresh_presence};
use crate::api::AppState;
use crate::app_core::{AppError, Core};
use crate::crypto::RoomId;
use crate::node::{Outbox, now_ms};
use crate::protocol::body::Avatar;
use crate::protocol::live::LiveBody;

/// 在节点任务里改队伍，再返回整份队伍页。
async fn act<F>(app: &AppHandle, f: F) -> Result<TeamView, String>
where
    F: FnOnce(&mut Core, &mut Outbox) -> Result<(), AppError> + Send + 'static,
{
    let node = app.state::<AppState>().node.clone();
    node.call(f)
        .await
        .ok_or(crate::tr!("网络模块停了", "The network module has stopped"))?
        .map_err(|e| e.to_string())?;
    refresh_names(app);
    refresh_presence(app);
    view(app).await
}

fn current(core: &Core) -> Result<RoomId, AppError> {
    core.current_team()?
        .map(|t| t.room_id)
        .ok_or(AppError::RoomNotFound)
}

#[tauri::command]
pub async fn team_view(app: AppHandle) -> Result<TeamView, String> {
    view(&app).await
}

#[tauri::command]
pub async fn team_create(app: AppHandle, title: String) -> Result<TeamView, String> {
    act(&app, move |core, out| {
        let c = core.create_room(&title, now_ms())?;
        out.broadcast(c.summary.room_id, c.events);
        Ok(())
    })
    .await
}

#[tauri::command]
pub async fn team_join(app: AppHandle, code: String) -> Result<TeamView, String> {
    act(&app, move |core, _| {
        core.join_room(&code, now_ms()).map(|_| ())
    })
    .await
}

/// 正在加入时取消。
#[tauri::command]
pub async fn team_cancel(app: AppHandle) -> Result<TeamView, String> {
    act(&app, |core, _| {
        let room = current(core)?;
        core.cancel_join(&room)
    })
    .await
}

/// 退出：先发 Bye，再写 Leave。
#[tauri::command]
pub async fn team_leave(app: AppHandle) -> Result<TeamView, String> {
    let view = act(&app, |core, out| {
        let room = current(core)?;
        out.live(room, LiveBody::Bye);
        let written = core.leave_room(&room, now_ms())?;
        out.broadcast(room, written);
        Ok(())
    })
    .await;
    app.state::<AppState>()
        .team
        .presence
        .lock()
        .expect("在线状态锁没有中毒")
        .clear();
    view
}

#[tauri::command]
pub async fn team_dissolve(app: AppHandle) -> Result<TeamView, String> {
    act(&app, |core, out| {
        let room = current(core)?;
        let written = core.dissolve_room(&room, now_ms())?;
        out.broadcast(room, written);
        Ok(())
    })
    .await
}

#[tauri::command]
pub async fn team_rename(app: AppHandle, title: String) -> Result<TeamView, String> {
    act(&app, move |core, out| {
        let room = current(core)?;
        let written = core.rename_room(&room, &title, now_ms())?;
        out.broadcast(room, written);
        Ok(())
    })
    .await
}

#[tauri::command]
pub async fn profile_set(
    app: AppHandle,
    nickname: String,
    avatar: Option<AvatarInput>,
) -> Result<TeamView, String> {
    let avatar = match avatar {
        None => None,
        Some(AvatarInput::Preset { icon, color }) => Some(Avatar::Preset { icon, color }),
        Some(AvatarInput::Image { data }) => Some(Avatar::Image(unbase64(&data)?)),
    };
    act(&app, move |core, out| {
        let written = core.set_profile(nickname.trim(), avatar, now_ms())?;
        if let Some(room) = core.current_team()?.map(|t| t.room_id) {
            out.broadcast(room, written);
        }
        Ok(())
    })
    .await
}
