pub struct AppendIterator<I, T>
where
    I: Iterator<Item = T>,
    T: Clone
{
    iter: I,
    appended: Vec<T>,
    index: usize,
    iter_done: bool,
}

impl<I, T> AppendIterator<I, T>
where
    I: Iterator<Item = T>,
    T: Clone
{
    pub fn new(iter: I, appended: Vec<T>) -> Self {
        Self {
            iter,
            appended,
            index: 0,
            iter_done: false,
        }
    }
}

impl<I, T> Iterator for AppendIterator<I, T>
where
    I: Iterator<Item = T>,
    T: Clone
{
    type Item = T;

    fn next(&mut self) -> Option<T> {
        if !self.iter_done {
            if let Some(v) = self.iter.next() {
                return Some(v);
            }
            self.iter_done = true;
        }

        if self.index < self.appended.len() {
            let v = self.appended[self.index].clone();
            self.index += 1;
            Some(v)
        } else {
            None
        }
    }

    fn for_each<F>(self, f: F)
    where
        Self: Sized,
        F: FnMut(Self::Item),
    {
        self.iter.chain(self.appended.into_iter()).for_each(f);
    }
}

pub struct PrependIterator<I, T>
where
    I: Iterator<Item = T>,
    T: Clone
{
    iter: I,
    prepended: Vec<T>,
    index: usize,
}

impl<I, T> PrependIterator<I, T>
where
    I: Iterator<Item = T>,
    T: Clone
{
    pub fn new(iter: I, prepended: Vec<T>) -> Self {
        Self {
            iter,
            prepended,
            index: 0,
        }
    }
}

impl<I, T> Iterator for PrependIterator<I, T>
where
    I: Iterator<Item = T>,
    T: Clone
{
    type Item = T;

    fn next(&mut self) -> Option<T> {
        if self.index < self.prepended.len() {
            let v = self.prepended[self.index].clone();
            self.index += 1;
            Some(v)
        } else {
            self.iter.next()
        }
    }

    fn for_each<F>(self, f: F)
    where
        Self: Sized,
        F: FnMut(Self::Item),
    {
        self.prepended.into_iter().chain(self.iter).for_each(f);
    }
}
