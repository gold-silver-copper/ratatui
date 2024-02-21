use std::{
    error::Error,
    io,
    time::{Duration, Instant},
    sync::Arc
};

use bevy::app::AppExit;
use bevy::app::App as BevyApp;
use bevy::prelude::*;
use std::sync::Mutex;
use ratatui::prelude::*;

use crate::{app::App as RatApp, ui};





pub fn run(ticky_rate: Duration, enhanced_graphics: bool)-> Result<(), Box<dyn Error>>  {

    // create app and run it
  //  let app = Arc::new(Mutex::new(RatApp::new("Crossterm Demo", enhanced_graphics)));


    static mut BAP: RatApp = RatApp::new("Crossterm Demo", true);
  //  static mut BEEP: Mutex<RatApp> = Mutex::new(BAP);
  //  static mut BOOP:Arc<Mutex<RatApp>> = Arc::new(BEEP);
  
    let res = run_app();
   
   // let res = run_app(app.clone().lock().unwrap(), ticky_rate);

    Ok(())
   



}



fn run_app(
) -> io::Result<()> {

    
  
    BevyApp::new()
    .add_plugins(DefaultPlugins)
    .add_plugins((RatatuiPlugin))
    .add_systems(Startup, camera_setup)
    .add_systems(PreUpdate, terminal_draw)
    .add_systems(Update, (keyboard_input))
    .run();

    Ok(())

}




fn camera_setup(mut commands: Commands) {
    commands.spawn(Camera2dBundle::default());

    let mut my_terminal = Terminal::new(BevyBackend::new(60, 20,40)).unwrap();


    my_terminal.clear();

    my_terminal.show_cursor();


    commands.spawn(my_terminal);
}

fn terminal_draw(mut terminal_query:  Query<(&mut Terminal<BevyBackend>)>) {

    let ra = BAP;

    let mut rat_term = terminal_query.get_single_mut().expect("More than one terminal with a bevybackend");

    let _ = rat_term.draw(|f| ui::draw(f, &mut *ra));


        *ra.on_tick();
    
}

fn keyboard_input(keys: Res<ButtonInput<KeyCode>>, mut exit: EventWriter<AppExit>,) {
    let ra = BAP;
    if keys.just_pressed(KeyCode::KeyQ) {
        exit.send(AppExit);
    }
    if keys.just_pressed(KeyCode::KeyH) {
        *ra.on_left();
    }
    if keys.just_pressed(KeyCode::KeyK) {
        *ra.on_up();
    }
    if keys.just_pressed(KeyCode::KeyL) {
        *ra.on_right();
    }
    if keys.just_pressed(KeyCode::KeyJ) {
        *ra.on_down();
    }
    if keys.just_pressed(KeyCode::KeyC) {
        *ra.on_key("c".chars().next().unwrap());
    }
    if keys.just_pressed(KeyCode::KeyT) {
        *ra.on_key("t".chars().next().unwrap());
    }
}





