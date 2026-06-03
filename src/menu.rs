use std::{
    io::{self, Read},
    process::{Command, Stdio},
    rc::Rc,
};

use mio::unix::pipe::{Receiver, Sender};

use crate::{
    actions::Action,
    config::{Config, Menu},
};

pub struct FuzzelMenu {
    menu: Rc<Menu<Rc<Action>>>,
    sender: Sender,
    receiver: Receiver,
    last_layer: Option<String>,
}

impl FuzzelMenu {
    pub fn new(menu: Rc<Menu<Rc<Action>>>, last_layer: Option<String>) -> Self {
        let mut child = Command::new("fuzzel")
            .arg("--dmenu")
            .arg("--hide-prompt")
            .arg("--index")
            .arg("--anchor=".to_owned() + &menu.anchor.to_string())
            .arg("--mesg=".to_owned() + &menu.message.as_ref().unwrap_or(&"TourBox:".to_string()))
            .arg("--select-index=".to_owned() + &menu.select.unwrap_or(0).to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("fuzzel should launch");

        let sender = Sender::from(child.stdin.take().expect("fuzzel stdin should open"));
        let receiver = Receiver::from(child.stdout.take().expect("fuzzel stdout should open"));

        Self { menu, sender, receiver, last_layer }
    }

    pub fn get_stdin(&self, config: &Config) -> String {
        let mut str = String::new();
        for entry in self.menu.entries.iter() {
            str.push_str(config.lookup_name(entry));
            str.push('\n');
        }
        str
    }

    pub fn sender(&mut self) -> &mut Sender {
        &mut self.sender
    }

    pub fn receiver(&mut self) -> &mut Receiver {
        &mut self.receiver
    }

    pub fn last_layer(&self) -> &Option<String> {
        &self.last_layer
    }

    pub fn read_action(&mut self) -> Result<Option<Rc<Action>>, io::Error> {
        let mut buf = String::new();
        self.receiver.read_to_string(&mut buf)?;
        let index = match buf.strip_suffix("\n") {
            Some(str) => str.parse::<usize>().unwrap(),
            None => return Ok(None),
        };
        Ok(self.menu.entries.get(index).cloned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // #[test]
    // fn test() {
    //     spawn_fuzzel();
    // }
}
