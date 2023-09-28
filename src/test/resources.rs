use {
    super::{Ctx, ENGINE},
    anyhow::{anyhow, Result},
    async_trait::async_trait,
    std::str,
    wasmtime::{
        component::{Component, Linker, Resource, ResourceAny},
        Store,
    },
    wasmtime_wasi::preview2::{command, Table, WasiCtxBuilder, WasiView},
};

#[tokio::test]
async fn resource_borrow_import() -> Result<()> {
    wasmtime::component::bindgen!({
        path: "src/test/wit",
        world: "resource-borrow-import-test",
        async: true
    });

    use componentize_py::test::resource_borrow_import::{self, Host, HostThing, Thing};

    struct MyThing(u32);

    #[async_trait]
    impl HostThing for Ctx {
        async fn new(&mut self, v: u32) -> Result<Resource<Thing>> {
            Ok(Resource::new_own(
                self.table_mut().push(Box::new(MyThing(v + 2)))?,
            ))
        }

        fn drop(&mut self, this: Resource<Thing>) -> Result<()> {
            Ok(self.table_mut().delete::<MyThing>(this.rep()).map(|_| ())?)
        }
    }

    #[async_trait]
    impl Host for Ctx {
        async fn foo(&mut self, this: Resource<Thing>) -> Result<u32> {
            Ok(self.table().get::<MyThing>(this.rep())?.0 + 3)
        }
    }

    let component = &super::make_component(
        include_str!("wit/resource-borrow-import.wit"),
        r#"
import resource_borrow_import_test
from resource_borrow_import_test.imports.resource_borrow_import import Thing, foo

class ResourceBorrowImportTest(resource_borrow_import_test.ResourceBorrowImportTest):
    def test(self, v: int) -> int:
        return foo(Thing(v + 1)) + 4
"#,
        None,
    )
    .await?;

    let mut linker = Linker::<Ctx>::new(&ENGINE);
    command::add_to_linker(&mut linker)?;
    resource_borrow_import::add_to_linker(&mut linker, |ctx| ctx)?;

    let mut table = Table::new();
    let wasi = WasiCtxBuilder::new()
        .inherit_stdout()
        .inherit_stderr()
        .build(&mut table)?;

    let mut store = Store::new(&ENGINE, Ctx { wasi, table });

    let (instance, _) = ResourceBorrowImportTest::instantiate_async(
        &mut store,
        &Component::new(&ENGINE, component)?,
        &linker,
    )
    .await?;

    assert_eq!(
        42 + 1 + 2 + 3 + 4,
        instance.call_test(&mut store, 42).await?
    );

    Ok(())
}

#[tokio::test]
async fn resource_borrow_export() -> Result<()> {
    wasmtime::component::bindgen!({
        path: "src/test/wit",
        world: "resource-borrow-export-test",
        async: true
    });

    let component = &super::make_component_all(
        include_str!("wit/resource-borrow-export.wit"),
        &[
            (
                "app.py",
                r#"
from resource_borrow_export_test import exports
from resource_borrow_export import Thing

class ResourceBorrowExport(exports.ResourceBorrowExport):
    def foo(self, v: Thing) -> int:
        return v.value + 2
"#,
            ),
            (
                "resource_borrow_export.py",
                r#"
from resource_borrow_export_test.exports import resource_borrow_export

class Thing(resource_borrow_export.Thing):
    def __init__(self, v: int):
        self.value = v + 1
"#,
            ),
        ],
        None,
    )
    .await?;

    let mut linker = Linker::<Ctx>::new(&ENGINE);
    command::add_to_linker(&mut linker)?;

    let mut table = Table::new();
    let wasi = WasiCtxBuilder::new()
        .inherit_stdout()
        .inherit_stderr()
        .build(&mut table)?;

    let mut store = Store::new(&ENGINE, Ctx { wasi, table });

    // TODO: use `wasmtime-wit-bindgen` to access guest resource once it's supported

    let instance = linker
        .instantiate_async(&mut store, &Component::new(&ENGINE, component)?)
        .await?;

    let instance_name = "componentize-py:test/resource-borrow-export";
    let mut exports = instance.exports(&mut store);
    let mut instance = exports
        .instance(instance_name)
        .ok_or_else(|| anyhow!("instance not found: {instance_name}"))?;
    let thing_new = instance.typed_func::<(u32,), (ResourceAny,)>("[constructor]thing")?;
    let foo = instance.typed_func::<(ResourceAny,), (u32,)>("foo")?;

    drop(exports);

    let thing1 = thing_new.call_async(&mut store, (42,)).await?.0;
    thing_new.post_return_async(&mut store).await?;

    assert_eq!(42 + 1 + 2, foo.call_async(&mut store, (thing1,)).await?.0);
    foo.post_return_async(&mut store).await?;

    Ok(())
}

#[tokio::test]
async fn resource_with_lists() -> Result<()> {
    wasmtime::component::bindgen!({
        path: "src/test/wit",
        world: "resource-with-lists-test",
        async: true
    });

    use componentize_py::test::resource_with_lists::{self, Host, HostThing, Thing};

    struct MyThing(Vec<u8>);

    #[async_trait]
    impl HostThing for Ctx {
        async fn new(&mut self, mut v: Vec<u8>) -> Result<Resource<Thing>> {
            v.extend(b" HostThing.new");
            Ok(Resource::new_own(
                self.table_mut().push(Box::new(MyThing(v)))?,
            ))
        }

        async fn foo(&mut self, this: Resource<Thing>) -> Result<Vec<u8>> {
            let mut v = self.table().get::<MyThing>(this.rep())?.0.clone();
            v.extend(b" HostThing.foo");
            Ok(v)
        }

        async fn bar(&mut self, this: Resource<Thing>, mut v: Vec<u8>) -> Result<()> {
            v.extend(b" HostThing.bar");
            self.table_mut().get_mut::<MyThing>(this.rep())?.0 = v;
            Ok(())
        }

        async fn baz(&mut self, mut v: Vec<u8>) -> Result<Vec<u8>> {
            v.extend(b" HostThing.baz");
            Ok(v)
        }

        fn drop(&mut self, this: Resource<Thing>) -> Result<()> {
            Ok(self.table_mut().delete::<MyThing>(this.rep()).map(|_| ())?)
        }
    }

    impl Host for Ctx {}

    let component = &super::make_component_all(
        include_str!("wit/resource-with-lists.wit"),
        &[
            (
                "app.py",
                r#"
from resource_with_lists_test import exports

class ResourceWithLists(exports.ResourceWithLists):
    pass
"#,
            ),
            (
                "resource_with_lists.py",
                r#"
from resource_with_lists_test.exports import resource_with_lists
from resource_with_lists_test.imports.resource_with_lists import Thing as HostThing
from typing import List

class Thing(resource_with_lists.Thing):
    def __init__(self, v: bytes):
        x = bytearray(v)
        x.extend(b" Thing.__init__")
        self.value = HostThing(bytes(x))

    def foo(self) -> bytes:
        x = bytearray(self.value.foo())
        x.extend(b" Thing.foo")
        return bytes(x)

    def bar(self, v: bytes):
        x = bytearray(v)
        x.extend(b" Thing.bar")
        self.value.bar(bytes(x))

    @staticmethod
    def baz(v: bytes) -> bytes:
        x = bytearray(v)
        x.extend(b" Thing.baz")
        y = bytearray(HostThing.baz(bytes(x)))
        y.extend(b" Thing.baz again")
        return bytes(y)
"#,
            ),
        ],
        None,
    )
    .await?;

    let mut linker = Linker::<Ctx>::new(&ENGINE);
    command::add_to_linker(&mut linker)?;
    resource_with_lists::add_to_linker(&mut linker, |ctx| ctx)?;

    let mut table = Table::new();
    let wasi = WasiCtxBuilder::new()
        .inherit_stdout()
        .inherit_stderr()
        .build(&mut table)?;

    let mut store = Store::new(&ENGINE, Ctx { wasi, table });

    // TODO: use `wasmtime-wit-bindgen` to access guest resource once it's supported

    let instance = linker
        .instantiate_async(&mut store, &Component::new(&ENGINE, component)?)
        .await?;

    let instance_name = "componentize-py:test/resource-with-lists";
    let mut exports = instance.exports(&mut store);
    let mut instance = exports
        .instance(instance_name)
        .ok_or_else(|| anyhow!("instance not found: {instance_name}"))?;
    let thing_new = instance.typed_func::<(Vec<u8>,), (ResourceAny,)>("[constructor]thing")?;
    let thing_foo = instance.typed_func::<(ResourceAny,), (Vec<u8>,)>("[method]thing.foo")?;
    let thing_bar = instance.typed_func::<(ResourceAny, Vec<u8>), ()>("[method]thing.bar")?;
    let thing_baz = instance.typed_func::<(Vec<u8>,), (Vec<u8>,)>("[static]thing.baz")?;

    drop(exports);

    let thing1 = thing_new.call_async(&mut store, (b"Hi".to_vec(),)).await?.0;
    thing_new.post_return_async(&mut store).await?;

    assert_eq!(
        "Hi Thing.__init__ HostThing.new HostThing.foo Thing.foo",
        str::from_utf8(&thing_foo.call_async(&mut store, (thing1,)).await?.0)?
    );
    thing_foo.post_return_async(&mut store).await?;

    thing_bar
        .call_async(&mut store, (thing1, b"Hola".to_vec()))
        .await?;
    thing_bar.post_return_async(&mut store).await?;

    assert_eq!(
        "Hola Thing.bar HostThing.bar HostThing.foo Thing.foo",
        str::from_utf8(&thing_foo.call_async(&mut store, (thing1,)).await?.0)?
    );
    thing_foo.post_return_async(&mut store).await?;

    assert_eq!(
        "Ohayo Gozaimas Thing.baz HostThing.baz Thing.baz again",
        str::from_utf8(
            &thing_baz
                .call_async(&mut store, (b"Ohayo Gozaimas".to_vec(),))
                .await?
                .0
        )?
    );
    thing_baz.post_return_async(&mut store).await?;

    Ok(())
}
