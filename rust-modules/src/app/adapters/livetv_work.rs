//! **The Live TV runner**: where the work the Live TV store asks for starts
//! (`plx_data::livetv::work`). The store, its virtual channels and its Tunarr keeper start no
//! thread and read no registry; [`launch`] starts each [`Work`] on the lane it names, hands its
//! body [`Registry`] (the [`Reach`] seam over the server registry), and wakes the frame when the
//! body is done so the next pump lands its answer. [`input`] is what the Bridge captures for each
//! step. This is the only place in the Live TV path that calls `spawn_small`, `submit_retained`,
//! `client_for` or `tape::active`.

use plx_data::livetv::work::{Lane, LiveTvInput, Reach, Work};
use plx_plex::plex::{Client, ServerId};

/// The server registry, as a [`Work`] body reaches it.
pub(crate) struct Registry;

impl Reach for Registry {
    fn servers(&self) -> Vec<ServerId> {
        plx_plex::plex::server_ids().collect()
    }
    fn current(&self) -> ServerId {
        plx_plex::plex::current_server()
    }
    fn client(&self, sid: ServerId) -> Option<&'static Client> {
        plx_plex::plex::client_for(sid)
    }
    fn machine(&self, machine_id: &str) -> Option<ServerId> {
        plx_plex::plex::id_of_machine(machine_id)
    }
}

/// What a Live TV step is handed: whether a tape controls the run, the current server, profile
/// and roster, and the channel the session remembers.
pub(crate) fn input() -> LiveTvInput {
    LiveTvInput {
        controlled: plx_data::stores::tape::active(),
        server: plx_plex::plex::current_server(),
        profile: plx_plex::plex::session::current_profile_key(),
        roster: plx_plex::plex::server_roster_gen(),
        remembered_channel: plx_plex::plex::session::peek().livetv_channel().to_owned(),
    }
}

/// Start every queued [`Work`]. A body that could not be started is dropped, which closes its
/// answer channel: the store's next pump reads the disconnect as a failure, as it would a worker
/// that died.
pub(crate) fn launch(work: Vec<Work>) {
    for w in work {
        let Work { name, lane, body } = w;
        match lane {
            Lane::Worker => {
                let _ = plx_base::task::spawn_small(name, move || {
                    body(&Registry);
                    plx_machine::present::wake_from_worker();
                });
            }
            Lane::Storage => {
                let _ = plx_base::storage_worker::submit_retained(move || {
                    body(&Registry);
                    plx_machine::present::wake_from_worker();
                });
            }
        }
    }
}
