//! In-memory kNN index. Brute-force cosine similarity for M2 (deterministic).
use crate::vector::{TfIdfVector, cosine_sim};
pub struct KnnIndex{entries:Vec<(String,TfIdfVector)>}
impl KnnIndex{pub fn new()->Self{Self{entries:Vec::new()}}pub fn insert(&mut self,id:String,v:TfIdfVector){self.entries.push((id,v));}pub fn search(&self,q:&TfIdfVector,k:usize)->Vec<(String,f64)>{let mut s:Vec<(String,f64)>=self.entries.iter().map(|(id,v)|(id.clone(),cosine_sim(q,v))).collect();s.sort_by(|a,b|b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal).then_with(||a.0.cmp(&b.0)));s.truncate(k);s}pub fn len(&self)->usize{self.entries.len()}pub fn is_empty(&self)->bool{self.entries.is_empty()}}
impl Default for KnnIndex{fn default()->Self{Self::new()}}
