/// Shared primitive types used by widgets and screens.

#[derive(Clone, Debug)]
pub struct Choice {
    pub label: String,
    pub value: String,
}

impl Choice {
    pub fn new(label: impl Into<String>) -> Self {
        let label = label.into();
        let value = label.clone();
        Self { label, value }
    }

    pub fn with_value(label: impl Into<String>, value: impl Into<String>) -> Self {
        Self { label: label.into(), value: value.into() }
    }
}

#[derive(Clone, Debug)]
pub enum ListItem {
    Choice(Choice),
    Separator(Option<String>),
}

/// What a widget did with the last key event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WidgetAction {
    None,
    Submit,
    Cancel,
    ShowHelp,
    AltAction,
    ShowDetails,
    Toggle,
}

/// Final value a widget produced once it's done.
#[derive(Clone, Debug)]
pub enum WidgetResult {
    SingleSelect(String),
    MultiSelect(Vec<String>),
    Text(String),
    Cancelled,
}

/// Generic async-load wrapper, used for things like "fetch subscriptions
/// from the network and show a spinner until they arrive".
pub enum LoadState<T> {
    Loading,
    Loaded(T),
    Error(String),
}
