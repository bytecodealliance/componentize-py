use {
    super::{Ctx, Tester, SEED},
    anyhow::{Error, Result},
    async_trait::async_trait,
    once_cell::sync::Lazy,
    std::str,
    wasmtime::{
        component::{Instance, InstancePre, Linker, Resource, ResourceAny},
        Store,
    },
    wasmtime_wasi::preview2::{command, WasiView},
};

wasmtime::component::bindgen!({
    path: "src/test/wit",
    world: "tests",
    async: true
});

const GUEST_CODE: &[(&str, &str)] = &[
    (
        "app.py",
        r#"
import tests
import resource_borrow_export
import resource_aggregates
import resource_alias1
import resource_borrow_in_record
from tests import exports, imports
from tests.imports import resource_borrow_import
from tests.imports import simple_import_and_export
from tests.exports import resource_alias2
from tests.types import Result, Ok
from typing import Tuple, List, Optional

class SimpleExport(exports.SimpleExport):
    def foo(self, v: int) -> int:
        return v + 3

class SimpleImportAndExport(exports.SimpleImportAndExport):
    def foo(self, v: int) -> int:
        return simple_import_and_export.foo(v) + 3

class ResourceImportAndExport(exports.ResourceImportAndExport):
    pass

class ResourceBorrowExport(exports.ResourceBorrowExport):
    def foo(self, v: resource_borrow_export.Thing) -> int:
        return v.value + 2

class ResourceWithLists(exports.ResourceWithLists):
    pass

class ResourceAggregates(exports.ResourceAggregates):
    def foo(
        self,
        r1: exports.resource_aggregates.R1,
        r2: exports.resource_aggregates.R2,
        r3: exports.resource_aggregates.R3,
        t1: Tuple[resource_aggregates.Thing, exports.resource_aggregates.R1],
        t2: Tuple[resource_aggregates.Thing],
        v1: exports.resource_aggregates.V1,
        v2: exports.resource_aggregates.V2,
        l1: List[resource_aggregates.Thing],
        l2: List[resource_aggregates.Thing],
        o1: Optional[resource_aggregates.Thing],
        o2: Optional[resource_aggregates.Thing],
        result1: Result[resource_aggregates.Thing, None],
        result2: Result[resource_aggregates.Thing, None]
    ) -> int:
        if o1 is None:
            host_o1 = None
        else:
            host_o1 = o1.value
        
        if o2 is None:
            host_o2 = None
        else:
            host_o2 = o2.value

        if isinstance(result1, Ok):
            host_result1 = Ok(result1.value.value)
        else:
            host_result1 = result1
        
        if isinstance(result2, Ok):
            host_result2 = Ok(result2.value.value)
        else:
            host_result2 = result2

        return imports.resource_aggregates.foo(
            imports.resource_aggregates.R1(r1.thing.value),
            imports.resource_aggregates.R2(r2.thing.value),
            imports.resource_aggregates.R3(r3.thing1.value, r3.thing2.value),
            (t1[0].value, imports.resource_aggregates.R1(t1[1].thing.value)),
            (t2[0].value,),
            imports.resource_aggregates.V1Thing(v1.value.value),
            imports.resource_aggregates.V2Thing(v2.value.value),
            list(map(lambda x: x.value, l1)),
            list(map(lambda x: x.value, l2)),
            host_o1,
            host_o2,
            host_result1,
            host_result2
        ) + 4

class ResourceAlias1(exports.ResourceAlias1):
    def a(self, f: exports.resource_alias1.Foo) -> List[resource_alias1.Thing]:
        return list(
            map(
                resource_alias1.wrap_thing,
                imports.resource_alias1.a(imports.resource_alias1.Foo(f.thing.value))
            )
        )

class ResourceAlias2(exports.ResourceAlias2):
    def b(self, f: exports.resource_alias2.Foo, g: exports.resource_alias1.Foo) -> List[resource_alias1.Thing]:
        return list(
            map(
                resource_alias1.wrap_thing,
                imports.resource_alias2.b(
                    imports.resource_alias2.Foo(f.thing.value),
                    exports.resource_alias1.Foo(g.thing.value)
                )
            )
        )

class ResourceBorrowInRecord(exports.ResourceBorrowInRecord):
    def test(self, a: List[exports.resource_borrow_in_record.Foo]) -> List[resource_borrow_in_record.Thing]:
        return list(
            map(
                resource_borrow_in_record.wrap_thing,
                imports.resource_borrow_in_record.test(
                    list(map(lambda x: imports.resource_borrow_in_record.Foo(x.thing.value), a))
                )
            )
        )

class Tests(tests.Tests):
    def test_resource_borrow_import(self, v: int) -> int:
        return resource_borrow_import.foo(resource_borrow_import.Thing(v + 1)) + 4

    def test_resource_alias(self, things: List[imports.resource_alias1.Thing]) -> List[imports.resource_alias1.Thing]:
       return things

    def add(self, a: imports.resource_floats.Float, b: imports.resource_floats.Float) -> imports.resource_floats.Float:
       return imports.resource_floats.Float(a.get() + b.get() + 5)
"#,
    ),
    (
        "resource_import_and_export.py",
        r#"
from tests.exports import resource_import_and_export
from tests.imports.resource_import_and_export import Thing as HostThing
from typing import Self

class Thing(resource_import_and_export.Thing):
    def __init__(self, v: int):
        self.value = HostThing(v + 7)

    def foo(self) -> int:
        return self.value.foo() + 3

    def bar(self, v: int):
        self.value.bar(v + 4)

    @staticmethod
    def baz(a: Self, b: Self) -> Self:
        return Thing(HostThing.baz(a.value, b.value).foo() + 9)
"#,
    ),
    (
        "resource_borrow_export.py",
        r#"
from tests.exports import resource_borrow_export

class Thing(resource_borrow_export.Thing):
    def __init__(self, v: int):
        self.value = v + 1
"#,
    ),
    (
        "resource_with_lists.py",
        r#"
from tests.exports import resource_with_lists
from tests.imports.resource_with_lists import Thing as HostThing
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
    (
        "resource_aggregates.py",
        r#"
from tests.exports import resource_aggregates
from tests.imports.resource_aggregates import Thing as HostThing

class Thing(resource_aggregates.Thing):
    def __init__(self, v: int):
        self.value = HostThing(v + 1)
"#,
    ),
    (
        "resource_alias1.py",
        r#"
from tests.exports import resource_alias1
from tests.imports.resource_alias1 import Thing as HostThing

class Thing(resource_alias1.Thing):
    def __init__(self, v: str):
        self.value = HostThing(v + " Thing.__init__")

    def get(self) -> str:
        return self.value.get() + " Thing.get"

def wrap_thing(thing: HostThing) -> Thing:
    mine = Thing.__new__(Thing)
    mine.value = thing
    return mine
"#,
    ),
    (
        "resource_floats_exports.py",
        r#"
from tests.exports import resource_floats_exports
from tests.imports.resource_floats_imports import Float as HostFloat
from typing import Self

class Float(resource_floats_exports.Float):
    def __init__(self, v: float):
        self.value = HostFloat(v + 1)

    def get(self) -> str:
        return self.value.get() + 3

    @staticmethod
    def add(a: Self, b: float) -> Self:
        return Float(HostFloat.add(a.value, b).get() + 5)
"#,
    ),
    (
        "resource_borrow_in_record.py",
        r#"
from tests.exports import resource_borrow_in_record
from tests.imports.resource_borrow_in_record import Thing as HostThing

class Thing(resource_borrow_in_record.Thing):
    def __init__(self, v: str):
        self.value = HostThing(v + " Thing.__init__")

    def get(self) -> str:
        return self.value.get() + " Thing.get"

def wrap_thing(thing: HostThing) -> Thing:
    mine = Thing.__new__(Thing)
    mine.value = thing
    return mine
"#,
    ),
];

struct Host;

#[async_trait]
impl super::Host for Host {
    type World = Tests;

    fn add_to_linker(linker: &mut Linker<Ctx>) -> Result<()> {
        command::add_to_linker(linker)?;
        Tests::add_to_linker(linker, |ctx| ctx)?;
        Ok(())
    }

    async fn instantiate_pre(
        store: &mut Store<Ctx>,
        pre: &InstancePre<Ctx>,
    ) -> Result<(Self::World, Instance)> {
        Ok(Tests::instantiate_pre(store, pre).await?)
    }
}

static TESTER: Lazy<Tester<Host>> =
    Lazy::new(|| Tester::<Host>::new(include_str!("wit/tests.wit"), GUEST_CODE, *SEED).unwrap());

#[test]
fn simple_export() -> Result<()> {
    TESTER.test(|world, store, runtime| {
        assert_eq!(
            42 + 3,
            runtime.block_on(
                world
                    .componentize_py_test_simple_export()
                    .call_foo(store, 42)
            )?
        );

        Ok(())
    })
}

#[test]
fn simple_import_and_export() -> Result<()> {
    #[async_trait]
    impl componentize_py::test::simple_import_and_export::Host for Ctx {
        async fn foo(&mut self, v: u32) -> Result<u32> {
            Ok(v + 2)
        }
    }

    TESTER.test(|world, store, runtime| {
        assert_eq!(
            42 + 2 + 3,
            runtime.block_on(
                world
                    .componentize_py_test_simple_import_and_export()
                    .call_foo(store, 42)
            )?
        );

        Ok(())
    })
}

#[test]
fn resource_import_and_export() -> Result<()> {
    use componentize_py::test::resource_import_and_export::{Host, HostThing, Thing};

    struct MyThing(u32);

    #[async_trait]
    impl HostThing for Ctx {
        async fn new(&mut self, v: u32) -> Result<Resource<Thing>> {
            Ok(self.table_mut().push(Box::new(MyThing(v + 8)))?)
        }

        async fn foo(&mut self, this: Resource<Thing>) -> Result<u32> {
            Ok(self.table().get::<MyThing>(&this)?.0 + 1)
        }

        async fn bar(&mut self, this: Resource<Thing>, v: u32) -> Result<()> {
            self.table_mut().get_mut::<MyThing>(&this)?.0 = v + 5;
            Ok(())
        }

        async fn baz(&mut self, a: Resource<Thing>, b: Resource<Thing>) -> Result<Resource<Thing>> {
            let a = self.table().get::<MyThing>(&a)?.0;
            let b = self.table().get::<MyThing>(&b)?.0;

            Ok(self.table_mut().push(Box::new(MyThing(a + b + 6)))?)
        }

        fn drop(&mut self, this: Resource<Thing>) -> Result<()> {
            Ok(self.table_mut().delete::<MyThing>(this).map(|_| ())?)
        }
    }

    impl Host for Ctx {}

    TESTER.test(|world, store, runtime| {
        runtime.block_on(async {
            let instance = world.componentize_py_test_resource_import_and_export();
            let thing = instance.thing();
            let thing1 = thing.call_constructor(&mut *store, 42).await?;

            assert_eq!(
                42 + 7 + 8 + 3 + 1,
                thing.call_foo(&mut *store, thing1).await?
            );

            thing.call_bar(&mut *store, thing1, 33).await?;

            assert_eq!(
                33 + 4 + 5 + 3 + 1,
                thing.call_foo(&mut *store, thing1).await?
            );

            let thing2 = thing.call_constructor(&mut *store, 81).await?;

            let thing3 = thing.call_baz(&mut *store, thing1, thing2).await?;

            assert_eq!(
                33 + 4 + 5 + 81 + 7 + 8 + 6 + 1 + 9 + 7 + 8 + 3 + 1,
                thing.call_foo(&mut *store, thing3).await?
            );

            Ok(())
        })
    })
}

#[test]
fn resource_borrow_import() -> Result<()> {
    use componentize_py::test::resource_borrow_import::{Host, HostThing, Thing};

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

    TESTER.test(|world, store, runtime| {
        assert_eq!(
            42 + 1 + 2 + 3 + 4,
            runtime.block_on(world.call_test_resource_borrow_import(store, 42))?
        );

        Ok(())
    })
}

#[test]
fn resource_borrow_export() -> Result<()> {
    TESTER.test(|world, store, runtime| {
        runtime.block_on(async {
            let instance = world.componentize_py_test_resource_borrow_export();
            let thing = instance.thing();
            let thing1 = thing.call_constructor(&mut *store, 42).await?;

            assert_eq!(42 + 1 + 2, instance.call_foo(&mut *store, thing1).await?);

            Ok(())
        })
    })
}

#[test]
fn resource_with_lists() -> Result<()> {
    use componentize_py::test::resource_with_lists::{Host, HostThing, Thing};

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

    TESTER.test(|world, store, runtime| {
        runtime.block_on(async {
            let instance = world.componentize_py_test_resource_with_lists();
            let thing = instance.thing();
            let thing1 = thing.call_constructor(&mut *store, b"Hi").await?;

            assert_eq!(
                "Hi Thing.__init__ HostThing.new HostThing.foo Thing.foo",
                str::from_utf8(&thing.call_foo(&mut *store, thing1).await?)?
            );

            thing.call_bar(&mut *store, thing1, b"Hola").await?;

            assert_eq!(
                "Hola Thing.bar HostThing.bar HostThing.foo Thing.foo",
                str::from_utf8(&thing.call_foo(&mut *store, thing1).await?)?
            );

            assert_eq!(
                "Ohayo Gozaimas Thing.baz HostThing.baz Thing.baz again",
                str::from_utf8(&thing.call_baz(&mut *store, b"Ohayo Gozaimas").await?)?
            );

            Ok(())
        })
    })
}

#[test]
fn resource_aggregates() -> Result<()> {
    {
        use componentize_py::test::resource_aggregates::{
            Host, HostThing, Thing, L1, L2, R1, R2, R3, T1, T2, V1, V2,
        };

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
            async fn foo(
                &mut self,
                r1: R1,
                r2: R2,
                r3: R3,
                t1: T1,
                t2: T2,
                v1: V1,
                v2: V2,
                l1: L1,
                l2: L2,
                o1: Option<Resource<Thing>>,
                o2: Option<Resource<Thing>>,
                result1: Result<Resource<Thing>, ()>,
                result2: Result<Resource<Thing>, ()>,
            ) -> Result<u32> {
                let V1::Thing(v1) = v1;
                let V2::Thing(v2) = v2;
                Ok(self.table_mut().get::<MyThing>(r1.thing.rep())?.0
                    + self.table_mut().get::<MyThing>(r2.thing.rep())?.0
                    + self.table_mut().get::<MyThing>(r3.thing1.rep())?.0
                    + self.table_mut().get::<MyThing>(r3.thing2.rep())?.0
                    + self.table_mut().get::<MyThing>(t1.0.rep())?.0
                    + self.table_mut().get::<MyThing>(t1.1.thing.rep())?.0
                    + self.table_mut().get::<MyThing>(t2.0.rep())?.0
                    + self.table_mut().get::<MyThing>(v1.rep())?.0
                    + self.table_mut().get::<MyThing>(v2.rep())?.0
                    + l1.into_iter().try_fold(0, |n, v| {
                        Ok::<_, Error>(self.table_mut().get::<MyThing>(v.rep())?.0 + n)
                    })?
                    + l2.into_iter().try_fold(0, |n, v| {
                        Ok::<_, Error>(self.table_mut().get::<MyThing>(v.rep())?.0 + n)
                    })?
                    + o1.map(|v| Ok::<_, Error>(self.table_mut().get::<MyThing>(v.rep())?.0))
                        .unwrap_or(Ok(0))?
                    + o2.map(|v| Ok::<_, Error>(self.table_mut().get::<MyThing>(v.rep())?.0))
                        .unwrap_or(Ok(0))?
                    + result1
                        .map(|v| Ok::<_, Error>(self.table_mut().get::<MyThing>(v.rep())?.0))
                        .unwrap_or(Ok(0))?
                    + result2
                        .map(|v| Ok::<_, Error>(self.table_mut().get::<MyThing>(v.rep())?.0))
                        .unwrap_or(Ok(0))?
                    + 3)
            }
        }
    }

    use exports::componentize_py::test::resource_aggregates::{R1, R2, R3, V1, V2};

    TESTER.test(|world, store, runtime| {
        runtime.block_on(async {
            let instance = world.componentize_py_test_resource_aggregates();
            let thing = instance.thing();

            let mut things = Vec::new();
            for n in 1..18 {
                things.push(thing.call_constructor(&mut *store, n).await?);
            }

            assert_eq!(
                (1..18).map(|n| n + 1 + 2).sum::<u32>() + 3 + 4,
                instance
                    .call_foo(
                        &mut *store,
                        R1 { thing: things[0] },
                        R2 { thing: things[1] },
                        R3 {
                            thing1: things[2],
                            thing2: things[3]
                        },
                        (things[4], R1 { thing: things[5] }),
                        (things[6],),
                        V1::Thing(things[7]),
                        V2::Thing(things[8]),
                        &vec![things[9], things[10]],
                        &vec![things[11], things[12]],
                        Some(things[13]),
                        Some(things[14]),
                        Ok(things[15]),
                        Ok(things[16])
                    )
                    .await?
            );

            Ok(())
        })
    })
}

#[test]
fn resource_alias() -> Result<()> {
    struct MyThing(String);

    {
        use componentize_py::test::resource_alias1::{Foo, Host, HostThing, Thing};

        #[async_trait]
        impl HostThing for Ctx {
            async fn new(&mut self, s: String) -> Result<Resource<Thing>> {
                Ok(Resource::new_own(
                    self.table_mut()
                        .push(Box::new(MyThing(s + " HostThing::new")))?,
                ))
            }

            async fn get(&mut self, this: Resource<Thing>) -> Result<String> {
                Ok(format!(
                    "{} HostThing.get",
                    self.table().get::<MyThing>(this.rep())?.0
                ))
            }

            fn drop(&mut self, this: Resource<Thing>) -> Result<()> {
                Ok(self.table_mut().delete::<MyThing>(this.rep()).map(|_| ())?)
            }
        }

        #[async_trait]
        impl Host for Ctx {
            async fn a(&mut self, f: Foo) -> Result<Vec<Resource<Thing>>> {
                Ok(vec![f.thing])
            }
        }
    }

    {
        use componentize_py::test::resource_alias2::{Bar, Foo, Host, Thing};

        #[async_trait]
        impl Host for Ctx {
            async fn b(&mut self, f: Foo, g: Bar) -> Result<Vec<Resource<Thing>>> {
                Ok(vec![f.thing, g.thing])
            }
        }
    }

    use exports::componentize_py::test::{resource_alias1, resource_alias2};

    TESTER.test(|world, store, runtime| {
        runtime.block_on(async {
            let thing1 = Resource::<componentize_py::test::resource_alias1::Thing>::new_own(
                store
                    .data_mut()
                    .table_mut()
                    .push(Box::new(MyThing("Ni Hao".to_string())))?,
            );

            fn host_things_to_strings(
                store: &mut Store<Ctx>,
                things: Vec<Resource<componentize_py::test::resource_alias1::Thing>>,
            ) -> Result<Vec<String>> {
                let mut strings = Vec::new();
                for thing in things {
                    strings.push(store.data().table().get::<MyThing>(thing.rep())?.0.clone());
                }

                Ok(strings)
            }

            let things = world
                .call_test_resource_alias(&mut *store, &[thing1])
                .await?;

            assert_eq!(
                vec!["Ni Hao".to_string()],
                host_things_to_strings(&mut *store, things)?
            );

            let instance = world.componentize_py_test_resource_alias1();
            let thing = instance.thing();
            let thing1 = thing.call_constructor(&mut *store, "Ciao").await?;

            async fn guest_things_to_strings(
                store: &mut Store<Ctx>,
                thing: &resource_alias1::GuestThing<'_>,
                things: Vec<ResourceAny>,
            ) -> Result<Vec<String>> {
                let mut strings = Vec::new();
                for t in things {
                    strings.push(thing.call_get(&mut *store, t).await?);
                }

                Ok(strings)
            }

            let things = instance
                .call_a(&mut *store, resource_alias1::Foo { thing: thing1 })
                .await?;

            assert_eq!(
                vec!["Ciao Thing.__init__ HostThing::new HostThing.get Thing.get".to_string()],
                guest_things_to_strings(&mut *store, &thing, things).await?
            );

            let instance = world.componentize_py_test_resource_alias2();
            let thing2 = thing.call_constructor(&mut *store, "Aloha").await?;

            let things = instance
                .call_b(
                    &mut *store,
                    resource_alias2::Foo { thing: thing1 },
                    resource_alias2::Bar { thing: thing2 },
                )
                .await?;

            assert_eq!(
                vec![
                    "Ciao Thing.__init__ HostThing::new HostThing.get Thing.get".to_string(),
                    "Aloha Thing.__init__ HostThing::new HostThing.get Thing.get".to_string()
                ],
                guest_things_to_strings(&mut *store, &thing, things).await?
            );

            Ok(())
        })
    })
}

#[test]
fn resource_floats() -> Result<()> {
    struct MyFloat(f64);

    {
        use resource_floats_imports::{Float, Host, HostFloat};

        #[async_trait]
        impl HostFloat for Ctx {
            async fn new(&mut self, v: f64) -> Result<Resource<Float>> {
                Ok(Resource::new_own(
                    self.table_mut().push(Box::new(MyFloat(v + 2_f64)))?,
                ))
            }

            async fn get(&mut self, this: Resource<Float>) -> Result<f64> {
                Ok(self.table().get::<MyFloat>(this.rep())?.0 + 4_f64)
            }

            async fn add(&mut self, a: Resource<Float>, b: f64) -> Result<Resource<Float>> {
                let a = self.table().get::<MyFloat>(a.rep())?.0;

                Ok(Resource::new_own(
                    self.table_mut().push(Box::new(MyFloat(a + b + 6_f64)))?,
                ))
            }

            fn drop(&mut self, this: Resource<Float>) -> Result<()> {
                Ok(self.table_mut().delete::<MyFloat>(this.rep()).map(|_| ())?)
            }
        }

        impl Host for Ctx {}
    }

    {
        use componentize_py::test::resource_floats::{Float, Host, HostFloat};

        #[async_trait]
        impl HostFloat for Ctx {
            async fn new(&mut self, v: f64) -> Result<Resource<Float>> {
                Ok(Resource::new_own(
                    self.table_mut().push(Box::new(MyFloat(v + 1_f64)))?,
                ))
            }

            async fn get(&mut self, this: Resource<Float>) -> Result<f64> {
                Ok(self.table().get::<MyFloat>(this.rep())?.0 + 3_f64)
            }

            fn drop(&mut self, this: Resource<Float>) -> Result<()> {
                Ok(self.table_mut().delete::<MyFloat>(this.rep()).map(|_| ())?)
            }
        }

        impl Host for Ctx {}
    }

    TESTER.test(|world, store, runtime| {
        runtime.block_on(async {
            let float1 = Resource::<componentize_py::test::resource_floats::Float>::new_own(
                store
                    .data_mut()
                    .table_mut()
                    .push(Box::new(MyFloat(42_f64)))?,
            );

            let float2 = Resource::<componentize_py::test::resource_floats::Float>::new_own(
                store
                    .data_mut()
                    .table_mut()
                    .push(Box::new(MyFloat(55_f64)))?,
            );

            let sum = world.call_add(&mut *store, float1, float2).await?;

            assert_eq!(
                42_f64 + 3_f64 + 55_f64 + 3_f64 + 5_f64 + 1_f64,
                store.data().table().get::<MyFloat>(sum.rep())?.0
            );

            let instance = world.resource_floats_exports();
            let float = instance.float();
            let float1 = float.call_constructor(&mut *store, 22_f64).await?;

            assert_eq!(
                22_f64 + 1_f64 + 2_f64 + 4_f64 + 3_f64,
                float.call_get(&mut *store, float1).await?
            );

            let result = float.call_add(&mut *store, float1, 7_f64).await?;

            assert_eq!(
                22_f64
                    + 1_f64
                    + 2_f64
                    + 7_f64
                    + 6_f64
                    + 4_f64
                    + 5_f64
                    + 1_f64
                    + 2_f64
                    + 4_f64
                    + 3_f64,
                float.call_get(&mut *store, result).await?
            );

            Ok(())
        })
    })
}

#[test]
fn resource_borrow_in_record() -> Result<()> {
    struct MyThing(String);

    {
        use componentize_py::test::resource_borrow_in_record::{Foo, Host, HostThing, Thing};

        #[async_trait]
        impl HostThing for Ctx {
            async fn new(&mut self, v: String) -> Result<Resource<Thing>> {
                Ok(Resource::new_own(
                    self.table_mut()
                        .push(Box::new(MyThing(v + " HostThing::new")))?,
                ))
            }

            async fn get(&mut self, this: Resource<Thing>) -> Result<String> {
                Ok(format!(
                    "{} HostThing.get",
                    self.table().get::<MyThing>(this.rep())?.0
                ))
            }

            fn drop(&mut self, this: Resource<Thing>) -> Result<()> {
                Ok(self.table_mut().delete::<MyThing>(this.rep()).map(|_| ())?)
            }
        }

        #[async_trait]
        impl Host for Ctx {
            async fn test(&mut self, list: Vec<Foo>) -> Result<Vec<Resource<Thing>>> {
                list.into_iter()
                    .map(|foo| {
                        let value = self.table().get::<MyThing>(foo.thing.rep())?.0.clone();

                        Ok(Resource::new_own(
                            self.table_mut()
                                .push(Box::new(MyThing(value + " HostThing::test")))?,
                        ))
                    })
                    .collect()
            }
        }
    }

    use exports::componentize_py::test::resource_borrow_in_record::{Foo, GuestThing};

    TESTER.test(|world, store, runtime| {
        runtime.block_on(async {
            let instance = world.componentize_py_test_resource_borrow_in_record();
            let thing = instance.thing();
            let thing1 = thing.call_constructor(&mut *store, "Bonjour").await?;
            let thing2 = thing.call_constructor(&mut *store, "mon cher").await?;

            let things = instance
                .call_test(&mut *store, &[Foo { thing: thing1 }, Foo { thing: thing2 }])
                .await?;

            async fn things_to_strings(
                store: &mut Store<Ctx>,
                thing: &GuestThing<'_>,
                things: Vec<ResourceAny>,
            ) -> Result<Vec<String>> {
                let mut strings = Vec::new();
                for t in things {
                    strings.push(thing.call_get(&mut *store, t).await?);
                }

                Ok(strings)
            }

            assert_eq!(
                vec![
                    "Bonjour Thing.__init__ HostThing::new \
                     HostThing::test HostThing.get Thing.get"
                        .to_string(),
                    "mon cher Thing.__init__ HostThing::new \
                     HostThing::test HostThing.get Thing.get"
                        .to_string()
                ],
                things_to_strings(&mut *store, &thing, things).await?
            );

            Ok(())
        })
    })
}
