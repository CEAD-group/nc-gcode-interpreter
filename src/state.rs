use crate::errors::ParsingError;
use std::collections::HashMap;

#[derive(Debug, Clone)]

pub struct State {
    pub axes: HashMap<String, f32>,
    pub symbol_table: HashMap<String, f32>,
    pub translation: HashMap<String, f32>,
    pub axis_identifiers: Vec<String>,
    pub iteration_limit: usize,
}

impl State {
    // Constructor for State to initialize the HashMaps with default values
    pub fn new(axis_identifiers: Vec<String>, iteration_limit: usize) -> Self {
        let mut symbols = HashMap::new();
        // Inserting default values
        symbols.insert("TRUE".to_string(), 1.0);
        symbols.insert("FALSE".to_string(), 0.0);

        // Initialize the translation map with zero values for each axis
        let mut translation = HashMap::new();
        for axis in &axis_identifiers {
            translation.insert(axis.clone(), 0.0);
        }

        State {
            axes: HashMap::new(),
            symbol_table: symbols,
            translation,
            axis_identifiers,
            iteration_limit,
        }
    }

    pub fn is_axis(&self, key: &str) -> bool {
        self.axis_identifiers.contains(&key.to_uppercase().to_string())
    }

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

    pub fn get_translation(&self, axis: &str) -> f32 {
        *self.translation.get(axis).unwrap_or(&0.0)
    }

    pub fn update_axis(&mut self, key: &str, value: f32, translate:bool) -> Result<f32, ParsingError> {
        let translation_value = self.get_translation(key);
        let mut updated_value = value;
        if translate {
            updated_value+= translation_value;
        } 
        self.axes.insert(key.to_string(), updated_value);
        Ok(updated_value)
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
