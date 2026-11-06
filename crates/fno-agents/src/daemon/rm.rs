use super::{
    handle_rm_with, run_claude_rm, run_mux_pane_kill, run_mux_pane_probe, Ctx, Request, Response,
};

pub(super) async fn handle_rm(ctx: &Ctx, req: &Request) -> Response {
    handle_rm_with(
        ctx,
        req,
        &crate::claude_roster::read_all_agents_union,
        &run_claude_rm,
        &super::rm_teardown::confirmed_claude_stop,
        &run_mux_pane_kill,
        &run_mux_pane_probe,
    )
    .await
}
