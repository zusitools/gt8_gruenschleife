extern crate zusi_tcp;

use std::env;
use std::str;
use std::cmp::max;
use std::net::TcpStream;
use std::sync::mpsc::{Sender, Receiver};
use std::sync::mpsc;
use std::thread;

use zusi_tcp::protocol;
use zusi_tcp::tcp::{TcpSendable, receive};
use zusi_tcp::node::{Node, Attribute};

// Tastaturzuordnungen (TZ)
const TZ_LM_TUER_1_OFFEN: u16 = 22; /* Fahrpultintern 01 */
const TZ_LM_TUER_2_OFFEN: u16 = 23; /* Fahrpultintern 02 */
const TZ_LM_TUER_3_OFFEN: u16 = 24; /* Fahrpultintern 03 */
const TZ_LM_TUER_4_OFFEN: u16 = 25; /* Fahrpultintern 04 */
const TZ_LM_GRUENSCHLEIFE: u16 = 26; /* Fahrpultintern 05 */
const TZ_LM_KINDERWAGEN: u16 = 27; /* Fahrpultintern 06 */

const TZ_TASTER_TUEREN_LINKS: u16 = 31; /* Fahrpultintern 10 */
const TZ_TASTER_TUEREN_RECHTS: u16 = 32; /* Fahrpultintern 11 */
const TZ_TASTER_TUEREN_BEIDE: u16 = 33; /* Fahrpultintern 12 */
// Kippschalter Tueren oeffnen (2)/sperren (1)/freigeben (0)
const TZ_KIPPSCHALTER_TUERFREIGABE: u16 = 34; /* Fahrpultintern 13 */
// Kippschalter Automatik ein (2)/- (1)/Alle Tueren zu (0)
const TZ_KIPPSCHALTER_AUTOMATIK_ALLE_ZU: u16 = 35; /* Fahrpultintern 14 */

// Die Zusi-Baugruppe "Tuersystem TAV" wollen wir selbst steuern. Sie lauscht daher
// auf eine fahrpultinterne Tastaturzuordnung. Damit die gewohnten Tastaturkommandos
// der Tastaturzuordnung "Tueren" trotzdem funktionieren, fangen wir sie ueber
// DATA_OPERATION ab und uebersetzen sie in Tastendruecke der passenden
// Kippschalter.
const TZ_TUEREN_INTERN: u16 = 36; /* Fahrpultintern 15 */
const TZ_TUEREN_EXTERN: u16 = 10;


// Tastaturkommandos fuer Tastaturzuordnung "Tueren".
// Das "_Up"-Kommando hat eine um 1 hoehere Nummer als das "_Down"-Kommando
const TK_TUEREN_TASTER_DOWN: u16 = 59;
const TK_TUEREN_LI_DOWN: u16 = 61;
const TK_TUEREN_RE_DOWN: u16 = 63;
const TK_TUEREN_ZU_DOWN: u16 = 65;


// Datenmodell

#[derive(Copy, Clone)]
struct State {
  // Lokale Kopie des Zusi-Status:
  //  - Status TAV-Seitenwahlschalter -- entspricht nicht dem Status der im Fuehrerstand sichtbaren Seitenwahltaster,
  //    weil in Zusi ueber den TAV-Seitenwahlschalter zusaetzlich die Freigabe gesteuert und zurueckgenommen wird.
  stellung_tav_seitenwahlschalter: u8, /* 0 aus, 1 Links, 2 Rechts, 3 beide */
  //  - Status Tueren (max(links+rechts))
  status_tueren: u8,
  //  - Rastenstellung Kippschalter "Tuerfreigabe"
  status_kippschalter_tuerfreigabe: i16, /* Rastenstellung */

  // Eigener (interner) Status
  status_seitenwahltaster: u8, /* 1 Links, 2 Rechts, 3 beide */
  seitenwahltaster_manuell_betaetigt: bool, /* Wurden die Seitenwahltaster seit dem letzten Tuerschliessen manuell betaetigt? */
}

// Vergleich Zusi-TAV / GT8-100D-Tuersystem
//  - Vor dem Anhalten
//    - Zusi: Seitenwahltaster auf "Links"/"Rechts"/"Beide" einstellen
//    - GT8: Seitenwahlknopf druecken (geht normalerweise automatisch)
//
//    - optional: Tueren freigeben (Taste T)
//    - GT8: Taster Tueren auf "oeffnen" oder "freigeben"
//
//    - Implementierung: Taster "oeffnen" oder "freigeben" stellt Tuerseitenvorwahl ein; Tuersystem steht auf "keine separate Freigabe noetig"
//
//  - Tueren schliessen:
//    - Zusi: Tueren schliessen nicht automatisch
//    - GT8: Tueren schliessen zeitgesteuert (Ausnahme: Kinderwagentaster gedrueckt)
//
//    - Implementierung: Kinderwagentaster immer gedrueckt :)
//
//  - Nach abgeschlossenem Fahrgastwechsel
//    - Zusi: Seitenwahltaster in Neutralstellung bringen -> Tueren schliessen (wenn Fahrgastwechsel abgeschlossen)
//    - GT8: Taster Tueren auf "sperren" -> Tueren schliessen (wenn Fahrgastwechsel abgeschlossen)
//
//    - Implementierung: Taster Tueren auf "sperren" stellt Seitenwahltaster in Neutralstellung
//
//  - Nach dem Anfahren:
//    - GT8: Tuerseitenvorwahl geht in Grundstellung (Tueren rechts?)
//
//    - Implementierung: v>0-Erkennung
//
//  - Zwangsschliessen:
//    - Zusi: Taster Zwangsschliessen
//    - GT8: Taster Tueren auf "Alle Tueren zu"
//
//    - Implementierung: "Alle Tueren zu" fuehrt zu Betaetigung von Zwangsschliessen
//
//  - Gruenschleife:
//    - Zusi: Phys. Groesse Gruenschleife ??
//    - GT8: wenn alle Tueren geschlossen
//
//    - Implementierung: Manuelle Ansteuerung des LM Gruenschleife
//
//  - Traktionssperre:
//    - Zusi: solange Tueren offen oder Seitenwahltaster nicht in Neutralstellung
//    - GT8: solange Gruenschleife nicht geschlossen (=Tueren offen) oder Taster Tueren nicht auf "sperren"
//
//    - Implementierung: Taster Tueren auf "sperren" stellt Seitenwahltaster in Neutralstellung
//
//  - Tuermelder/-taster:
//    - Zusi: blinken beim Schliessen
//    - GT8: blinken nicht (LM aus <=> Tuer zu)
//
//    - Implementierung: Manuelle Ansteuerung der Tuer-LM

fn make_command_input_node(tastaturzuordnung: u16, tastaturkommando: u16) -> Node {
  Node { id: 0x0001 /* Tastatureingabe */, children: vec![], attributes: vec![
    Attribute::from_u16(0x0001 /* Tastaturzuordnung */, tastaturzuordnung),
    Attribute::from_u16(0x0002 /* Tastaturkommando */, tastaturkommando),
  ]}
}

fn make_switch_input_node(tastaturzuordnung: u16, pos: u16) -> Node {
  Node { id: 0x0001 /* Tastatureingabe */, children: vec![], attributes: vec![
    Attribute::from_u16(0x0001 /* Tastaturzuordnung */, tastaturzuordnung),
    Attribute::from_u16(0x0003 /* Tastaturaktion */, 7 /* Absolute Raste */),
    Attribute::from_u16(0x0004 /* Schalterposition */, pos),
  ]}
}

fn main() {
  // Verbindungsaufbau (synchron)
  let server_port = env::args().nth(1).unwrap_or("127.0.0.1:1436".to_string());
  let mut stream = match TcpStream::connect(&*server_port) {
    Ok(s) => s,
    Err(e) => { println!("Fehler bei der Verbindung zu {}: {}", server_port, e); std::process::exit(-1) },
  };

  if let Err(e) = protocol::send_hello("GT8-100D/2S-M", "0.1.0", &mut stream) {
    println!("Fehler beim Schicken des HELLO-Befehls: {}", e);
    std::process::exit(-1)
  }

  if let Err(e) = protocol::send_needed_data(&vec![
        0x01, /* Geschwindigkeit */
        0x66, /* Status Tueren */
      ], &vec![], true /* Fst-Bedienung */, &mut stream) {
    println!("Fehler beim Schicken des NEEDED_DATA-Befehls: {}", e);
    std::process::exit(-1)
  }

  // Daten empfangen in eigenem Thread (das Senden erfolgt dagegen synchron im Haupt-Thread)
  let (tx, rx): (Sender<Node>, Receiver<Node>) = mpsc::channel();
  let mut stream_rx = stream.try_clone().expect("Fehler beim Klonen des Streams");
  thread::spawn(move || {
    loop {
      match receive(&mut stream_rx) {
        Ok(node) => { tx.send(node).expect("Fehler bei der Kommunikation zwischen Threads") }
        Err(e) => { println!("Fehler beim Empfangen der Daten von Zusi: {}", e); break; },
      };
    }
  });

  let mut prev: Option<State> = None;

  loop {
    match rx.recv() {
      Err(_) => { break; }
      Ok(rec) => {
        let mut curr = match prev {
          // TODO: auch nach Neustarten des Fahrplans, Aktivieren Autopilot etc. testen
          None => State {
              stellung_tav_seitenwahlschalter: 0,
              status_tueren: 0,
              status_kippschalter_tuerfreigabe: 1,  // Mittelstellung
              status_seitenwahltaster: 2,
              seitenwahltaster_manuell_betaetigt: false,
            },
          Some(prev) => prev,
        };

        let mut inputs: Vec<Node> = Vec::new();  // An Zusi zu sendende Tastendruecke

        // Verarbeite Daten von Zusi
        if rec.id == 0x2 { /* Client-Anwendung 02 */
          for c2 in rec.children.iter() { match c2.id {
            0xA /* DATA_FTD */ => {
              for c3 in c2.children.iter().filter(|c| c.id == 0x66 /* Status Tueren */) {
                if let Some(Ok(status_tueren_links)) = c3.find_attribute_excl(&[0x2]).map(|a| a.as_u8()) {
                  if let Some(Ok(status_tueren_rechts)) = c3.find_attribute_excl(&[0x3]).map(|a| a.as_u8()) {
                    curr.status_tueren = max(status_tueren_links, status_tueren_rechts);
                  }
                }
                if let Some(Ok(stellung_tav_seitenwahlschalter)) = c3.find_attribute_excl(&[0x5]).map(|a| a.as_u8()) {
                  curr.stellung_tav_seitenwahlschalter = stellung_tav_seitenwahlschalter;
                }
              }
              if let Some(Ok(v_akt)) = c2.find_attribute_excl(&[0x1 /* Geschwindigkeit */]).map(|a| a.as_f32()) {
                if v_akt > (5.0 / 3.6) && !curr.seitenwahltaster_manuell_betaetigt {
                  // Nach dem Anfahren Seitenwahltaster auf Rechts zurueckstellen
                  curr.seitenwahltaster_manuell_betaetigt = true;
                  curr.status_seitenwahltaster = 2;
                }
              }
            },
            0xB /* DATA_OPERATION */ => {
              for c3 in c2.children.iter() { match c3.id {
                0x1 /* Betaetigungsvorgang */ => {
                  if let Some(Ok(tastaturzuordnung)) = c3.find_attribute_excl(&[0x1]).map(|a| a.as_u16()) {
                    if let Some(Ok(tastaturkommando)) = c3.find_attribute_excl(&[0x2]).map(|a| a.as_u16()) {
                      if tastaturzuordnung == TZ_TUEREN_INTERN {
                        // Tueren intern: Wir koennen davon ausgehen, dass das keine echte Benutzereingabe
                        // ist, sondern von uns stammt.
                        // Gedrueckte Tasten wieder loslassen, sobald Zusi das Druecken bestaetigt hat
                        if tastaturkommando == TK_TUEREN_LI_DOWN || tastaturkommando == TK_TUEREN_RE_DOWN || tastaturkommando == TK_TUEREN_ZU_DOWN {
                          inputs.push(make_command_input_node(TZ_TUEREN_INTERN, tastaturkommando + 1 /* *_Up */));
                        }
                      } else if tastaturzuordnung == TZ_TUEREN_EXTERN {
                        // Auf Tastaturkommandos fuer Tueren in Zusi reagieren und sie in
                        // Tastaturkommandos fuer die benutzerdefinierten Schalter uebersetzen
                        if tastaturkommando == TK_TUEREN_TASTER_DOWN {
                          inputs.push(make_switch_input_node(TZ_KIPPSCHALTER_TUERFREIGABE, if curr.status_kippschalter_tuerfreigabe == 0 { 1 /* Mittelstellung */ } else { 0 /* Freigeben */ }));
                        } else if tastaturkommando == TK_TUEREN_LI_DOWN {
                          // Rechts -> Beide -> Links -> Rechts
                          curr.status_seitenwahltaster = match curr.status_seitenwahltaster {
                            1 => 3,
                            2 => 1,
                            3 => 2,
                            _ => curr.status_seitenwahltaster
                          }
                        } else if tastaturkommando == TK_TUEREN_RE_DOWN {
                          // Links -> Beide -> Rechts -> Links
                          curr.status_seitenwahltaster = match curr.status_seitenwahltaster {
                            1 => 2,
                            2 => 3,
                            3 => 1,
                            _ => curr.status_seitenwahltaster
                          }
                        } else if tastaturkommando == TK_TUEREN_ZU_DOWN {
                          inputs.push(make_switch_input_node(TZ_KIPPSCHALTER_AUTOMATIK_ALLE_ZU, 0 /* Alle Tueren zu */));
                        } else if tastaturkommando == TK_TUEREN_ZU_DOWN + 1 /* _UP */ {
                          inputs.push(make_switch_input_node(TZ_KIPPSCHALTER_AUTOMATIK_ALLE_ZU, 1 /* Mittelstellung */));
                        }
                      }
                    }
                  }
                },
                0x2 /* Kombischalter Hebelposition */ => {
                  if let Some(Ok(kombischalter_name)) = c3.find_attribute_excl(&[0x1]).map(|a| str::from_utf8(&a.value)) {
                    if let Some(Ok(kombischalter_raste)) = c3.find_attribute_excl(&[0x3]).map(|a| a.as_i16()) {
                      if kombischalter_name == "Tuerfreigabe" {
                        // Kippschalter Tuerfreigabe wirkt auf Seitenwahlschalter in Zusi
                        curr.status_kippschalter_tuerfreigabe = kombischalter_raste;
                      } else if kombischalter_name == "Tueren Automatik/Alle zu" {
                        if kombischalter_raste == 0 {
                          // Alle zu -> Sende Tuerschliessbefehl
                          inputs.push(make_command_input_node(TZ_TUEREN_INTERN, TK_TUEREN_ZU_DOWN));
                        }
                      } else if kombischalter_name == "Tuerseitenvorwahl links" {
                        curr.seitenwahltaster_manuell_betaetigt = true;
                        if kombischalter_raste == 1 {
                          curr.status_seitenwahltaster = 1;
                        }
                      } else if kombischalter_name == "Tuerseitenvorwahl rechts" {
                        curr.seitenwahltaster_manuell_betaetigt = true;
                        if kombischalter_raste == 1 {
                          curr.status_seitenwahltaster = 2;
                        }
                      } else if kombischalter_name == "Tuerseitenvorwahl links+rechts" {
                        curr.seitenwahltaster_manuell_betaetigt = true;
                        if kombischalter_raste == 1 {
                          curr.status_seitenwahltaster = 3;
                        }
                      }
                    }
                  }
                },
                _ => ()
              }}
            },
            _ => ()
          }}
        }

        // Verarbeite Aenderungen im Datenmodell
        if Some(curr.status_tueren) != prev.map(|s| s.status_tueren) {
          // Ansteuerung der Tuer-Leuchtmelder und des LM Gruenschleife
          let tuer_lm_an = if curr.status_tueren == 0 { 0 } else { 1 };
          inputs.push(make_switch_input_node(TZ_LM_TUER_1_OFFEN, tuer_lm_an));
          inputs.push(make_switch_input_node(TZ_LM_TUER_2_OFFEN, tuer_lm_an));
          inputs.push(make_switch_input_node(TZ_LM_TUER_3_OFFEN, tuer_lm_an));
          inputs.push(make_switch_input_node(TZ_LM_TUER_4_OFFEN, tuer_lm_an));
          inputs.push(make_switch_input_node(TZ_LM_KINDERWAGEN, tuer_lm_an));
          inputs.push(make_switch_input_node(TZ_LM_GRUENSCHLEIFE, if curr.status_tueren == 0 { 1 } else { 0 }));
          if curr.status_tueren == 0 {
            curr.seitenwahltaster_manuell_betaetigt = false;
          }
        }

        if Some(curr.status_seitenwahltaster) != prev.map(|s| s.status_seitenwahltaster) {
          // Ansteuerung der Seitenwahltaster-LM
          inputs.push(make_switch_input_node(TZ_TASTER_TUEREN_LINKS, if curr.status_seitenwahltaster == 1 { 1 } else { 0 }));
          inputs.push(make_switch_input_node(TZ_TASTER_TUEREN_RECHTS, if curr.status_seitenwahltaster == 2 { 1 } else { 0 }));
          inputs.push(make_switch_input_node(TZ_TASTER_TUEREN_BEIDE, if curr.status_seitenwahltaster == 3 { 1 } else { 0 }));
        }

        if Some(curr.status_kippschalter_tuerfreigabe) != prev.map(|s| s.status_kippschalter_tuerfreigabe)
            || Some(curr.status_seitenwahltaster) != prev.map(|s| s.status_seitenwahltaster) {
          // Der TAV-Seitenwahlschalter kann nicht per absoluter Raste angesteuert werden.
          let stellung_tav_seitenwahlschalter_soll = if curr.status_kippschalter_tuerfreigabe == 1 { 0 } else { curr.status_seitenwahltaster };
          println!("Seitenwahlschalter ist: {} soll: {}", curr.stellung_tav_seitenwahlschalter, stellung_tav_seitenwahlschalter_soll);

          if (curr.stellung_tav_seitenwahlschalter & 1) != (stellung_tav_seitenwahlschalter_soll & 1) {
            inputs.push(make_command_input_node(TZ_TUEREN_INTERN, TK_TUEREN_LI_DOWN));
          }
          if (curr.stellung_tav_seitenwahlschalter & 2) != (stellung_tav_seitenwahlschalter_soll & 2) {
            inputs.push(make_command_input_node(TZ_TUEREN_INTERN, TK_TUEREN_RE_DOWN));
          }
        }

        // Sende Eingaben an Zusi
        if inputs.len() > 0 {
          Node { id: 0x0002 /* Client-Anwendung 02 */, attributes: vec![], children: vec![
            Node { id: 0x010a /* Befehl INPUT */, attributes: vec![], children: inputs },
          ]}.send(&mut stream).unwrap();
        }

        prev = Some(curr);
      }
    }
  }
}
