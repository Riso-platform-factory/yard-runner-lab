use gpui::{Menu, MenuItem};

pub fn menus(product: &yard_product::Product) -> Vec<Menu> {
    vec![
        Menu::new(product.name).items(vec![MenuItem::action(
            format!("Quit {}", product.name),
            yard_shell::Quit,
        )]),
        Menu::new("File").items(vec![MenuItem::action("New Window", yard_shell::NewWindow)]),
        Menu::new("View").items(vec![
            MenuItem::action("Zoom", yard_shell::ToggleZoom),
            MenuItem::action("Hide Node", yard_shell::Hide),
            MenuItem::separator(),
            MenuItem::action("Select Next Tab", yard_shell::SelectNextTab),
            MenuItem::action("Select Previous Tab", yard_shell::SelectPreviousTab),
            MenuItem::separator(),
            MenuItem::action("Focus Left", yard_shell::FocusLeft),
            MenuItem::action("Focus Right", yard_shell::FocusRight),
            MenuItem::action("Focus Up", yard_shell::FocusUp),
            MenuItem::action("Focus Down", yard_shell::FocusDown),
        ]),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menus_use_the_product_and_list_every_action_in_order() {
        let menus = menus(&yard_product::Product {
            name: "Example",
            version: "1",
            identifier: "com.example.app",
        });
        assert_eq!(
            menus
                .iter()
                .map(|menu| menu.name.as_ref())
                .collect::<Vec<_>>(),
            ["Example", "File", "View"]
        );
        assert!(menus.iter().all(|menu| !menu.disabled));
        let actual: Vec<Vec<(&str, &str)>> = menus
            .iter()
            .map(|menu| {
                menu.items
                    .iter()
                    .map(|item| match item {
                        MenuItem::Action { name, action, .. } => (name.as_ref(), action.name()),
                        MenuItem::Separator => ("", ""),
                        _ => panic!("unexpected menu item"),
                    })
                    .collect()
            })
            .collect();
        assert_eq!(
            actual,
            vec![
                vec![("Quit Example", "yard::Quit")],
                vec![("New Window", "yard::NewWindow")],
                vec![
                    ("Zoom", "yard::ToggleZoom"),
                    ("Hide Node", "yard::Hide"),
                    ("", ""),
                    ("Select Next Tab", "yard::SelectNextTab"),
                    ("Select Previous Tab", "yard::SelectPreviousTab"),
                    ("", ""),
                    ("Focus Left", "yard::FocusLeft"),
                    ("Focus Right", "yard::FocusRight"),
                    ("Focus Up", "yard::FocusUp"),
                    ("Focus Down", "yard::FocusDown"),
                ]
            ]
        );
    }
}
