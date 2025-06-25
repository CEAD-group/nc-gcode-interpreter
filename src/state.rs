use crate::errors::ParsingError;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct State {
    pub axes: HashMap<String, f32>,
    pub symbol_table: HashMap<String, f32>,
    pub translation: HashMap<String, f32>,
    pub axis_identifiers: Vec<String>,
    pub iteration_limit: usize,
    pub axis_index_map: Option<HashMap<String, usize>>,
    /// Store line offsets for efficient error reporting
    line_offsets: Vec<usize>,
    /// Store the input text for error messages
    input: String,
}

impl State {
    /// Creates a new State with the given axis identifiers and configuration.
    /// 
    /// # Arguments
    /// 
    /// * `axis_identifiers` - List of valid axis names (e.g., ["X", "Y", "Z", "E"])
    /// * `iteration_limit` - Maximum number of iterations for loops
    /// * `axis_index_map` - Optional mapping of axis names to array indices (e.g., {"E": 4})
    pub fn new(axis_identifiers: Vec<String>, iteration_limit: usize, axis_index_map: Option<HashMap<String, usize>>) -> Self {
        let mut symbols = HashMap::new();
        symbols.insert("TRUE".to_string(), 1.0);
        symbols.insert("FALSE".to_string(), 0.0);

        let mut translation = HashMap::new();
        for axis in &axis_identifiers {
            translation.insert(axis.clone(), 0.0);
        }

        // Validate axis_index_map if provided
        if let Some(map) = &axis_index_map {
            for axis in map.keys() {
                if !axis_identifiers.contains(&axis.to_uppercase()) {
                    panic!("Axis '{}' in axis_index_map is not a valid axis", axis);
                }
            }
        }

        State {
            axes: HashMap::new(),
            symbol_table: symbols,
            translation,
            axis_identifiers,
            iteration_limit,
            axis_index_map,
            line_offsets: Vec::new(),
            input: String::new(),
        }
    }

    /// Sets the input text and pre-calculates line offsets for efficient access
    pub fn set_input(&mut self, input: String) {
        self.line_offsets = input
            .match_indices('\n')
            .map(|(i, _)| i)
            .collect();
        self.input = input;
    }

    /// Gets a line from the input by line number (1-based indexing)
    pub fn get_line(&self, line_no: usize) -> Option<&str> {
        if line_no == 0 {
            return None;
        }
        let start = if line_no == 1 {
            0
        } else {
            self.line_offsets.get(line_no - 2).map(|&i| i + 1)?
        };
        let end = self.line_offsets
            .get(line_no - 1)
            .copied()
            .unwrap_or(self.input.len());
        Some(&self.input[start..end])
    }

    /// Checks if a given key is a valid axis identifier
    pub fn is_axis(&self, key: &str) -> bool {
        self.axis_identifiers.contains(&key.to_uppercase())
    }

    /// Updates the translation value for an axis
    pub fn update_translation(&mut self, axis: &str, value: f32) -> Result<(), ParsingError> {
        if self.is_axis(axis) {
            self.translation.insert(axis.to_string(), value);
            Ok(())
        } else {
            Err(ParsingError::UnexpectedAxis {
                axis: axis.to_string(),
                axes: self.axis_identifiers.join(", "),
            })
        }
    }

    /// Gets the translation value for an axis
    pub fn get_translation(&self, axis: &str) -> f32 {
        *self.translation.get(axis).unwrap_or(&0.0)
    }

    /// Updates an axis value, optionally applying translation
    pub fn update_axis(&mut self, key: &str, value: f32, translate: bool) -> Result<f32, ParsingError> {
        let translation_value = self.get_translation(key);
        let updated_value = if translate {
            value + translation_value
        } else {
            value
        };
        self.axes.insert(key.to_string(), updated_value);
        Ok(updated_value)
    }

    /// Gets the array index for an axis, if a mapping exists
    pub fn get_axis_index(&self, axis: &str, line_no: usize, preview: &str) -> Result<usize, ParsingError> {
        if let Some(map) = &self.axis_index_map {
            map.get(axis)
                .copied()
                .ok_or_else(|| ParsingError::MissingAxisMapping {
                    line_no,
                    preview: preview.to_string(),
                    axis: axis.to_string(),
                })
        } else {
            Err(ParsingError::MissingAxisMapping {
                line_no,
                preview: preview.to_string(),
                axis: axis.to_string(),
            })
        }
    }

    #[allow(dead_code)]
    pub fn to_python_dict(&self) -> HashMap<String, HashMap<String, f32>> {
        let mut result = HashMap::new();

        result.insert("axes".to_string(), self.axes.clone());
        result.insert("symbol_table".to_string(), self.symbol_table.clone());
        result.insert("translation".to_string(), self.translation.clone());

        result
    }
}
