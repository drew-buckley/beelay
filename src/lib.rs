use std::collections::HashMap;

use log::warn;

pub mod common;
pub mod core;
pub mod mqtt_client;
pub mod api;
pub mod service;
pub mod frontend;

pub fn refine_filters(
    switch_names: &Vec<String>,
    proposed_filters: &HashMap<String, Vec<String>>
) -> HashMap<String, Vec<String>> {
    let mut filters = HashMap::with_capacity(proposed_filters.len());
    for (filter_name, proposed_filter) in proposed_filters {
        let mut filter = Vec::with_capacity(proposed_filter.len());
        for switch_name in proposed_filter {
            if switch_names.contains(switch_name) {
                filter.push(switch_name.clone());
            }
            else {
                warn!("Switch name, \"{}\", in filter, \"{}\", not in master list; excluding",
                    switch_name, filter_name);
            }
        }

        filters.insert(filter_name.clone(), filter);
    }

    filters
}
