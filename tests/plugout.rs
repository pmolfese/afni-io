//! Legacy plugout TCP transport, including the two-stage handshake and
//! fragmented/coalesced NUL framing.
#![cfg(feature = "talk")]

use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::thread;
use std::time::Duration;

use afni_io::plugout::{
    Acknowledgement, Client, ClientConfig, Error, Limits, Registration, Server, ACK_BAD, ACK_OK,
};
use afni_io::talk::{Ports, AFNI_PLUGOUT_TCP_0, PLUGOUT_DRIVE_PORT};

fn test_limits() -> Limits {
    Limits {
        connect_timeout: Duration::from_secs(2),
        io_timeout: Duration::from_secs(2),
        ..Limits::default()
    }
}

fn unused_port() -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    listener.local_addr().unwrap().port()
}

fn read_c_string(stream: &mut TcpStream) -> Vec<u8> {
    let mut bytes = Vec::new();
    loop {
        let mut byte = [0];
        stream.read_exact(&mut byte).unwrap();
        if byte[0] == 0 {
            return bytes;
        }
        bytes.push(byte[0]);
    }
}

#[test]
fn ports_select_official_control_and_driver_channels() {
    let defaults = Ports::new(None).unwrap();
    assert_eq!(defaults.get(AFNI_PLUGOUT_TCP_0), Some(7955));
    assert_eq!(defaults.get(PLUGOUT_DRIVE_PORT), Some(8099));
    assert_eq!(defaults.plugout_control(), 7955);
    assert_eq!(defaults.plugout_drive(), 8099);

    let offset = Ports::new(Some(3000)).unwrap();
    assert_eq!(offset.plugout_control(), 3009);
    assert_eq!(offset.plugout_drive(), 3016);
}

#[test]
fn client_and_server_preserve_command_order_and_ack_status() {
    let server = Server::bind(("127.0.0.1", 0), test_limits()).unwrap();
    let control_port = server.local_addr().unwrap().port();
    let data_port = unused_port();
    let receiver = thread::spawn(move || {
        let mut session = server.accept().unwrap();
        assert_eq!(session.registration().name, "integration-test");

        let first = session.receive().unwrap().unwrap();
        assert_eq!(first.drive_afni(), Some("OPEN_WINDOW A.axialimage"));
        assert!(matches!(session.receive(), Err(Error::InvalidState(_))));
        session.acknowledge(Acknowledgement::Ok).unwrap();

        let second = session.receive().unwrap().unwrap();
        assert_eq!(second.drive_afni(), Some("NO_SUCH_COMMAND"));
        session.acknowledge(Acknowledgement::Bad).unwrap();
    });

    let mut config = ClientConfig::new("127.0.0.1", control_port, data_port);
    config.name = "integration-test".into();
    config.limits = test_limits();
    let mut client = Client::connect(&config).unwrap();
    assert_eq!(
        client.send_drive_afni("OPEN_WINDOW A.axialimage").unwrap(),
        Acknowledgement::Ok
    );
    assert_eq!(
        client.send_drive_afni("NO_SUCH_COMMAND").unwrap(),
        Acknowledgement::Bad
    );
    receiver.join().unwrap();
}

#[test]
fn fragmented_registration_and_coalesced_commands_are_framed() {
    let server = Server::bind(("127.0.0.1", 0), test_limits()).unwrap();
    let control_port = server.local_addr().unwrap().port();
    let data_port = unused_port();
    let receiver = thread::spawn(move || {
        let mut session = server.accept().unwrap();
        let first = session.receive().unwrap().unwrap();
        assert_eq!(first.drive_afni(), Some("ONE"));
        session.acknowledge(Acknowledgement::Ok).unwrap();
        let second = session.receive().unwrap().unwrap();
        assert_eq!(second.drive_afni(), Some("TWO"));
        session.acknowledge(Acknowledgement::Ok).unwrap();
    });

    let mut control = TcpStream::connect(("127.0.0.1", control_port)).unwrap();
    let mut registration = Registration::tcp("fragmented", "127.0.0.1", data_port)
        .unwrap()
        .encode()
        .unwrap();
    *registration.last_mut().unwrap() = b';';
    for byte in registration {
        control.write_all(&[byte]).unwrap();
    }
    let mut ack = [0; 4];
    control.read_exact(&mut ack).unwrap();
    assert_eq!(ack, ACK_OK);
    drop(control);

    let mut data = TcpStream::connect(("127.0.0.1", data_port)).unwrap();
    data.write_all(b"DRIVE_").unwrap();
    data.write_all(b"AFNI ONE\0DRIVE_AFNI TWO\0").unwrap();
    data.read_exact(&mut ack).unwrap();
    assert_eq!(ack, ACK_OK);
    data.read_exact(&mut ack).unwrap();
    assert_eq!(ack, ACK_OK);
    receiver.join().unwrap();
}

#[test]
fn malformed_registration_gets_bad_ack() {
    let server = Server::bind(("127.0.0.1", 0), test_limits()).unwrap();
    let control_port = server.local_addr().unwrap().port();
    let receiver = thread::spawn(move || server.accept().unwrap_err());

    let mut control = TcpStream::connect(("127.0.0.1", control_port)).unwrap();
    control.write_all(b"PONAME incomplete\0").unwrap();
    let mut ack = [0; 4];
    control.read_exact(&mut ack).unwrap();
    assert_eq!(ack, ACK_BAD);
    assert!(matches!(
        receiver.join().unwrap(),
        Error::MalformedRegistration(_)
    ));
}

#[test]
fn data_port_collision_gets_bad_ack() {
    let occupied = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let data_port = occupied.local_addr().unwrap().port();
    let server = Server::bind(("127.0.0.1", 0), test_limits()).unwrap();
    let control_port = server.local_addr().unwrap().port();
    let receiver = thread::spawn(move || server.accept().unwrap_err());

    let mut control = TcpStream::connect(("127.0.0.1", control_port)).unwrap();
    let registration = Registration::tcp("collision", "127.0.0.1", data_port)
        .unwrap()
        .encode()
        .unwrap();
    control.write_all(&registration).unwrap();
    let mut ack = [0; 4];
    control.read_exact(&mut ack).unwrap();
    assert_eq!(ack, ACK_BAD);
    assert!(matches!(receiver.join().unwrap(), Error::Io { .. }));
}

#[test]
fn malformed_data_ack_is_rejected() {
    let control = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let control_port = control.local_addr().unwrap().port();
    let data_port = unused_port();
    let mock = thread::spawn(move || {
        let (mut control_stream, _) = control.accept().unwrap();
        let registration = read_c_string(&mut control_stream);
        let mut terminated = registration;
        terminated.push(0);
        Registration::parse(&terminated).unwrap();
        let data = TcpListener::bind(("127.0.0.1", data_port)).unwrap();
        control_stream.write_all(&ACK_OK).unwrap();
        let _ = control_stream.shutdown(Shutdown::Both);

        let (mut data_stream, _) = data.accept().unwrap();
        assert_eq!(read_c_string(&mut data_stream), b"DRIVE_AFNI REDRAW");
        data_stream.write_all(b"HUH\0").unwrap();
    });

    let mut config = ClientConfig::new("127.0.0.1", control_port, data_port);
    config.limits = test_limits();
    let mut client = Client::connect(&config).unwrap();
    assert!(matches!(
        client.send_drive_afni("REDRAW"),
        Err(Error::InvalidAcknowledgement(bytes)) if bytes == *b"HUH\0"
    ));
    mock.join().unwrap();
}

#[test]
fn partial_data_frame_reports_premature_close() {
    let server = Server::bind(("127.0.0.1", 0), test_limits()).unwrap();
    let control_port = server.local_addr().unwrap().port();
    let data_port = unused_port();
    let receiver = thread::spawn(move || {
        let mut session = server.accept().unwrap();
        session.receive().unwrap_err()
    });

    let mut control = TcpStream::connect(("127.0.0.1", control_port)).unwrap();
    let registration = Registration::tcp("partial", "127.0.0.1", data_port)
        .unwrap()
        .encode()
        .unwrap();
    control.write_all(&registration).unwrap();
    let mut ack = [0; 4];
    control.read_exact(&mut ack).unwrap();
    assert_eq!(ack, ACK_OK);

    let mut data = TcpStream::connect(("127.0.0.1", data_port)).unwrap();
    data.write_all(b"DRIVE_AFNI REDRAW").unwrap();
    data.shutdown(Shutdown::Write).unwrap();
    assert!(matches!(
        receiver.join().unwrap(),
        Error::PrematureClose {
            phase: afni_io::plugout::Phase::Data,
            received: 17
        }
    ));
}

#[test]
fn server_accepts_sequential_reconnections() {
    let server = Server::bind(("127.0.0.1", 0), test_limits()).unwrap();
    let control_port = server.local_addr().unwrap().port();
    let receiver = thread::spawn(move || {
        for expected in ["FIRST", "SECOND"] {
            let mut session = server.accept().unwrap();
            let message = session.receive().unwrap().unwrap();
            assert_eq!(message.drive_afni(), Some(expected));
            session.acknowledge(Acknowledgement::Ok).unwrap();
        }
    });

    for command in ["FIRST", "SECOND"] {
        let data_port = unused_port();
        let mut config = ClientConfig::new("127.0.0.1", control_port, data_port);
        config.limits = test_limits();
        let mut client = Client::connect(&config).unwrap();
        assert_eq!(
            client.send_drive_afni(command).unwrap(),
            Acknowledgement::Ok
        );
    }
    receiver.join().unwrap();
}

#[test]
fn accept_timeout_is_bounded() {
    let limits = Limits {
        connect_timeout: Duration::from_millis(25),
        ..test_limits()
    };
    let server = Server::bind(("127.0.0.1", 0), limits).unwrap();
    assert!(matches!(
        server.accept(),
        Err(Error::Timeout {
            phase: afni_io::plugout::Phase::ControlConnect,
            ..
        })
    ));
}
