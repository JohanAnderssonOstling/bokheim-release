//! Keep generated SQL methods visible between library features, but private
//! to this crate. SQL text and parameter declarations remain in feature files.
macro_rules! impl_sql {
    ($sql_name:ident = $({ $kind:tt $name:ident ($($variant:tt $param:ident $ptype:tt)*) $doc:literal $s:tt $($text:tt)+ }),+) => {
        pub(crate) trait $sql_name {
            $(include_sqlite_sql::decl_method! { $kind $name $doc () () $($param $variant $ptype)* })+
        }
        impl $sql_name for rusqlite::Connection {
            $(include_sqlite_sql::impl_method! { $kind $name () () ($($param $variant $ptype)*) => ($($variant $param)*) $($text)+ })+
        }
    };
}
