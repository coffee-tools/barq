#!/bin/bash

capitalize() {
    echo "$1" | awk '{print toupper(substr($0,1,1)) tolower(substr($0,2))}'
}

script_dir=$(dirname "$(realpath "$0")")

mkdir -p "$script_dir/src/algorithms"

read -p "Enter the name of your strategy: " script_name

capitalized_script_name=$(capitalize "$script_name")

if [ -f "$script_dir/src/algorithms/${script_name}.rs" ]; then
    echo "Error: $script_dir/src/algorithms/${script_name}.rs already exists."
    exit 1
fi

cat <<EOL > "$script_dir/src/algorithms/${script_name}.rs"
use anyhow::Result;

use crate::strategy::{RouteHop, RouteInput, RouteOutput, Strategy};

const DEFAULT_DELAY: u64 = 9;

pub struct $capitalized_script_name;

impl $capitalized_script_name {
    pub fn new() -> Self {
        $capitalized_script_name 
    }
}

impl Default for $capitalized_script_name {
    fn default() -> Self {
        $capitalized_script_name::new()
    }
}

impl Strategy for $capitalized_script_name {
    fn can_apply(&self, input: &RouteInput) -> Result<bool> {
        // TODO: implement logic which checks if this strategy can be applied to given network graph
    }

    fn route(&self, input: &RouteInput) -> Result<RouteOutput> {
        // TODO: implement routing algorithm
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        graph::{Edge, NetworkGraph, Node},
        strategy::Router,
    };

    #[test]
    fn test_direct_routing() {
        // TODO: write tests for $capitalized_script_name strategy
    }
}
EOL

echo "Strategy ${script_name} created in $script_dir/src/algorithms/${script_name}.rs"
