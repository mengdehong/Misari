// Copyright (C) 2025  Tassilo Horn <tsdh@gnu.org>
//
// This program is free software: you can redistribute it and/or modify it
// under the terms of the GNU General Public License as published by the Free
// Software Foundation, either version 3 of the License, or (at your option)
// any later version.
//
// This program is distributed in the hope that it will be useful, but WITHOUT
// ANY WARRANTY; without even the implied warranty of MERCHANTABILITY or
// FITNESS FOR A PARTICULAR PURPOSE.  See the GNU General Public License for
// more details.
//
// You should have received a copy of the GNU General Public License along with
// this program.  If not, see <https://www.gnu.org/licenses/>.

use niri_ipc::socket::Socket;
use niri_ipc::{Request, Response};

pub fn query_niri(req: Request) -> Result<Response, String> {
    match Socket::connect() {
        Ok(mut socket) => match socket.send(req) {
            Ok(Ok(response)) => Ok(response),
            Ok(Err(e)) => Err(e),
            Err(err) => Err(err.to_string()),
        },
        Err(err) => {
            log::error!("Cannot connect to niri: {err:?}");
            Err(err.to_string())
        }
    }
}
