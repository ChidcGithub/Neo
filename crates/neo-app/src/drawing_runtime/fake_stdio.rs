//! Standalone std-only test peer, NOT a runtime implementation or deployable board.
//! Compile with rustc into target; used only by the explicitly ignored smoke test.
use std::io::{self, BufRead, Write};

fn main() {
    let executable = std::env::current_exe().unwrap();
    let name = executable.file_stem().unwrap().to_string_lossy();
    let blocked = name.contains("blocked");
    let malformed = name.contains("malformed");
    let eof = name.contains("eof");
    assert_eq!(
        std::env::args().skip(1).collect::<Vec<_>>(),
        ["--gui", "--hosted"]
    );
    // Larger than a pipe buffer: a client not draining stderr concurrently deadlocks.
    io::stderr().write_all(&vec![b'x'; 256 * 1024]).unwrap();
    println!(
        r#"{{"version":1,"type":"event","event":"ready","data":{{"app":"drawing","methods":["configure","get_state","show","hide","close","math.calculate"],"headless":false,"has_window":true,"max_line_bytes":65536,"revision_scope":"document"}}}}"#
    );
    io::stdout().flush().unwrap();
    for line in io::stdin().lock().lines() {
        let line = line.unwrap();
        // The test pins generation=900; this peer does not implement arbitrary RPC.
        if line.contains("\"method\":\"configure\"") {
            assert!(line.contains("\"desktop_capture_allowed\":false"));
            assert!(line.contains("\"agent_allowed\":false"));
            println!(
                r#"{{"version":1,"type":"response","id":"neo:900:0","ok":true,"result":{{"app":"drawing","document_id":"fake-doc","page_id":"fake-page","revision":0,"revision_scope":"document","dirty":true,"configured":true,"closed":false,"connected":true,"close_pending":false,"desired_visible":true,"effective_visible":true,"visible":false,"has_window":true,"window_status":"pending","hidden_confirmed":false,"permissions":{{"classroom_safe":true,"desktop_capture_allowed":false,"agent_allowed":false}}}}}}"#
            );
        } else if line.contains("\"method\":\"close\"") {
            assert!(line.contains("\"params\":{}"));
            println!(
                r#"{{"version":1,"type":"response","id":"neo:900:1","ok":false,"error":{{"code":"unsaved_changes","message":"synthetic dirty document"}}}}"#
            );
        } else {
            panic!("unexpected test request");
        }
        io::stdout().flush().unwrap();
        if malformed || eof {
            // Let the host observe Ready, then fail without requiring it to poll events.
            std::thread::sleep(std::time::Duration::from_millis(150));
            if eof {
                return;
            }
            println!(
                r#"{{"version":1,"type":"request","id":"neo:wrong-direction","method":"host.capture_region","params":{{}}}}"#
            );
            io::stdout().flush().unwrap();
        }
        if blocked {
            // Keep stdin open without reading. Exit naturally after the host has had
            // time to block in write_all and Drop; no board GUI or force termination.
            std::thread::sleep(std::time::Duration::from_secs(2));
            return;
        }
    }
    // EOF confirms the host closed stdin, rather than killing this peer.
}
