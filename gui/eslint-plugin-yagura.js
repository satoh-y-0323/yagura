/**
 * The gui's own lint rules. They read types, so they run only where the
 * config gives the parser a program.
 *
 * A brand is a string type made unforgeable by a `unique symbol` key
 * (`DisplayLine` in `src/display/untrusted-text.ts`): only its maker module may
 * claim a value is one. The rules here find a brand by that key's symbol, so a
 * member a mapped type copies from it is the brand too, while a type that merely
 * shares a brand's name is not one.
 */
import ts from 'typescript';

/** How deep a type is walked before it is taken to hold every brand. */
const MAX_DEPTH = 12;

const OPAQUE_FLAGS = ts.TypeFlags.Any | ts.TypeFlags.Unknown;
// Template literal and string mapping types are strings that no brand key can sit on.
const STRING_SHAPED_FLAGS = ts.TypeFlags.TemplateLiteral | ts.TypeFlags.StringMapping;

function slashed(path) {
  return path.replace(/\\/g, '/');
}

function isFile(fileName, relativePath) {
  const path = slashed(fileName);
  return path === relativePath || path.endsWith(`/${relativePath}`);
}

/**
 * What a rule needs to know about brands in one program: which brands a type
 * holds, and the brand types themselves.
 * @param {ts.Program} program
 * @param {{ type: string, key: string, declaredIn: string, maker: string, multiline?: boolean }[]} brands
 */
function createBrandReader(program, brands) {
  const checker = program.getTypeChecker();
  const brandTypes = new Map();

  function brandType(brand) {
    if (!brandTypes.has(brand)) {
      const file = program.getSourceFiles().find((f) => isFile(f.fileName, brand.declaredIn));
      const alias = file?.statements.find(
        (statement) => ts.isTypeAliasDeclaration(statement) && statement.name.text === brand.type
      );
      brandTypes.set(brand, alias === undefined ? null : checker.getTypeAtLocation(alias.name));
    }
    return brandTypes.get(brand);
  }

  function isKeyDeclaration(declaration, brand) {
    return (
      ts.isPropertySignature(declaration) &&
      ts.isComputedPropertyName(declaration.name) &&
      ts.isIdentifier(declaration.name.expression) &&
      declaration.name.expression.text === brand.key &&
      isFile(declaration.getSourceFile().fileName, brand.declaredIn)
    );
  }

  let brandsByKeyName;

  // A `unique symbol` key's member name is unique to that symbol, and a mapped type that
  // copies the member keeps the name though it has no declaration of its own.
  function brandOfProperty(property) {
    if (brandsByKeyName === undefined) {
      brandsByKeyName = new Map();
      for (const brand of brands) {
        const branded = brandType(brand);
        const key =
          branded === null
            ? undefined
            : checker
                .getPropertiesOfType(branded)
                .find((p) => (p.declarations ?? []).some((d) => isKeyDeclaration(d, brand)));
        if (key !== undefined) {
          brandsByKeyName.set(key.escapedName, brand);
        }
      }
    }
    return brandsByKeyName.get(property.escapedName);
  }

  /** The brand `type` is a value of: a string type that carries a brand's key. */
  function brandOfText(type) {
    if (
      !type.isIntersection() ||
      !type.types.some((member) => (member.flags & ts.TypeFlags.StringLike) !== 0)
    ) {
      return undefined;
    }
    return checker
      .getPropertiesOfType(type)
      .map(brandOfProperty)
      .find((brand) => brand !== undefined);
  }

  // A key declared without a value (`declare const k: unique symbol`) only marks a type:
  // nothing can be stored under it, so what it is typed as is no place for a value.
  function isPhantomKey(property) {
    return (property.declarations ?? []).some((declaration) => {
      if (
        !ts.isPropertySignature(declaration) ||
        !ts.isComputedPropertyName(declaration.name) ||
        !ts.isIdentifier(declaration.name.expression)
      ) {
        return false;
      }
      const key = checker.getSymbolAtLocation(declaration.name.expression)?.valueDeclaration;
      return (
        key !== undefined &&
        ts.isVariableDeclaration(key) &&
        !key.getSourceFile().isDeclarationFile &&
        (ts.getCombinedModifierFlags(key) & ts.ModifierFlags.Ambient) !== 0
      );
    });
  }

  function isLibraryType(type) {
    const declarations = (type.aliasSymbol ?? type.getSymbol())?.declarations ?? [];
    return (
      declarations.length > 0 &&
      declarations.every((declaration) => declaration.getSourceFile().isDeclarationFile)
    );
  }

  function typeArgumentsOf(type) {
    const fromReference =
      (ts.getObjectFlags(type) & ts.ObjectFlags.Reference) !== 0
        ? checker.getTypeArguments(type)
        : [];
    return [...fromReference, ...(type.aliasTypeArguments ?? [])];
  }

  // A generic type is a brand wherever its constraint lets one in.
  function brandsAcceptedBy(type, found) {
    const constraint = checker.getBaseConstraintOfType(type);
    for (const brand of brands) {
      const branded = brandType(brand);
      if (
        constraint === undefined ||
        (constraint.flags & OPAQUE_FLAGS) !== 0 ||
        branded === null ||
        checker.isTypeAssignableTo(branded, constraint)
      ) {
        found.add(brand);
      }
    }
  }

  /** The types inside an object type where a value of a brand could sit. */
  function innerTypesOf(type) {
    // A library type is walked by what it was instantiated with, not its members.
    if (isLibraryType(type) || checker.isArrayType(type) || checker.isTupleType(type)) {
      return typeArgumentsOf(type);
    }
    return [
      ...checker
        .getPropertiesOfType(type)
        .filter((property) => !isPhantomKey(property))
        .map((property) => checker.getTypeOfSymbol(property)),
      ...checker.getIndexInfosOfType(type).map((info) => info.type),
      ...[...type.getCallSignatures(), ...type.getConstructSignatures()].map((signature) =>
        signature.getReturnType()
      ),
      ...typeArgumentsOf(type),
    ];
  }

  function visitObject(type, depth, seen, found) {
    for (const property of checker.getPropertiesOfType(type)) {
      const brand = brandOfProperty(property);
      if (brand !== undefined) {
        found.add(brand);
      }
    }
    for (const inner of innerTypesOf(type)) {
      visit(inner, depth + 1, seen, found);
    }
  }

  function visit(type, depth, seen, found) {
    if (depth > MAX_DEPTH) {
      brands.forEach((brand) => found.add(brand));
      return;
    }
    if (seen.has(type) || (type.flags & (OPAQUE_FLAGS | STRING_SHAPED_FLAGS)) !== 0) {
      return;
    }
    seen.add(type);
    if ((type.flags & ts.TypeFlags.Never) !== 0) {
      // `never` flows into any brand's place.
      brands.forEach((brand) => found.add(brand));
    } else if (type.isUnionOrIntersection()) {
      type.types.forEach((member) => visit(member, depth + 1, seen, found));
    } else if ((type.flags & ts.TypeFlags.Instantiable) !== 0) {
      brandsAcceptedBy(type, found);
    } else if ((type.flags & ts.TypeFlags.Object) !== 0) {
      visitObject(type, depth, seen, found);
    }
  }

  /** The brands `type` holds anywhere a value of a brand could sit. */
  function brandsIn(type) {
    const found = new Set();
    visit(type, 0, new Set(), found);
    return found;
  }

  /** The element types of an array, tuple, `Set` or `Map` that can be written to. */
  function writableElementsOf(type) {
    const name = type.getSymbol()?.escapedName;
    if (checker.isArrayType(type)) {
      return name === 'Array' ? checker.getTypeArguments(type) : undefined;
    }
    if (checker.isTupleType(type)) {
      return type.target.readonly ? undefined : checker.getTypeArguments(type);
    }
    return (name === 'Set' || name === 'Map') && isLibraryType(type)
      ? typeArgumentsOf(type)
      : undefined;
  }

  // A rest parameter's array is made new for each call and shared with no one, so what it
  // takes is its elements.
  function parameterTypesOf(parameter) {
    const type = checker.getTypeOfSymbol(parameter);
    const declared = parameter.valueDeclaration;
    const isRest =
      declared !== undefined && ts.isParameter(declared) && declared.dotDotDotToken !== undefined;
    return isRest && (checker.isArrayType(type) || checker.isTupleType(type))
      ? checker.getTypeArguments(type)
      : [type];
  }

  // A declared interface, class or type literal is checked at its own members, so the
  // container path stops there instead of reporting every place that uses it.
  function isDeclaredShape(type) {
    return (type.getSymbol()?.declarations ?? []).some(
      (declaration) =>
        ts.isInterfaceDeclaration(declaration) ||
        ts.isClassLike(declaration) ||
        ts.isTypeLiteralNode(declaration) ||
        ts.isEnumDeclaration(declaration)
    );
  }

  /**
   * Walks the container path of a declared type: union and intersection members,
   * container elements, a library type's type arguments, signatures' parameters
   * and results, and the members of a type with no declaration of its own.
   * Brands are counted only inside a writable array, tuple, `Set` or `Map`
   * (or from the start when `inside`). `generics` counts a type parameter whose
   * constraint takes a brand; `never` counts `never`.
   */
  function walkContainerPath(type, { generics, never, inside }) {
    const result = { brands: new Set(), tooDeep: false };
    const seen = [new Set(), new Set()];
    function reach(current, depth, within) {
      if (depth > MAX_DEPTH) {
        result.tooDeep = true;
        return;
      }
      const seenHere = seen[within ? 1 : 0];
      if (seenHere.has(current)) {
        return;
      }
      seenHere.add(current);
      if (within) {
        const brand = brandOfText(current);
        if (brand !== undefined) {
          result.brands.add(brand);
          return;
        }
        if ((current.flags & ts.TypeFlags.Never) !== 0 && never) {
          brands.forEach((b) => result.brands.add(b));
          return;
        }
        if ((current.flags & ts.TypeFlags.Instantiable) !== 0 && generics) {
          brandsAcceptedBy(current, result.brands);
          return;
        }
      }
      if ((current.flags & (OPAQUE_FLAGS | STRING_SHAPED_FLAGS)) !== 0) {
        return;
      }
      if (current.isUnionOrIntersection()) {
        current.types.forEach((member) => reach(member, depth + 1, within));
        return;
      }
      if ((current.flags & ts.TypeFlags.Object) === 0) {
        return;
      }
      const elements = writableElementsOf(current);
      if (elements !== undefined) {
        elements.forEach((element) => reach(element, depth + 1, true));
        return;
      }
      if (checker.isArrayType(current) || checker.isTupleType(current) || isLibraryType(current)) {
        typeArgumentsOf(current).forEach((argument) => reach(argument, depth + 1, within));
        return;
      }
      if (isDeclaredShape(current)) {
        return;
      }
      for (const signature of [
        ...current.getCallSignatures(),
        ...current.getConstructSignatures(),
      ]) {
        reach(signature.getReturnType(), depth + 1, within);
        for (const parameter of signature.getParameters()) {
          for (const taken of parameterTypesOf(parameter)) {
            reach(taken, depth + 1, within);
          }
        }
      }
      for (const property of checker.getPropertiesOfType(current)) {
        if (!isPhantomKey(property)) {
          reach(checker.getTypeOfSymbol(property), depth + 1, within);
        }
      }
      for (const info of checker.getIndexInfosOfType(current)) {
        reach(info.type, depth + 1, within);
      }
    }
    reach(type, 0, inside);
    return result;
  }

  /**
   * True if `parameter` sits where a caller hands a value in: somewhere in an
   * argument's type that is not on the receiving side of a function the
   * caller passes. What a passed function's type predicate narrows to is its
   * output, like its result. Conditional and mapped types are not looked into.
   */
  function handedInBy(type, parameter, positive, depth, seen) {
    if (type === parameter) {
      return positive;
    }
    const key = `${positive}`;
    const seenAs = seen.get(type) ?? new Set();
    if (depth > MAX_DEPTH || seenAs.has(key)) {
      return false;
    }
    seenAs.add(key);
    seen.set(type, seenAs);
    const next = (inner, sign) => handedInBy(inner, parameter, sign, depth + 1, seen);
    if (type.isUnionOrIntersection()) {
      return type.types.some((member) => next(member, positive));
    }
    if ((type.flags & ts.TypeFlags.Object) === 0) {
      return false;
    }
    if (checker.isArrayType(type) || checker.isTupleType(type)) {
      return checker.getTypeArguments(type).some((argument) => next(argument, positive));
    }
    const signatures = [...type.getCallSignatures(), ...type.getConstructSignatures()];
    const narrowsTo = (signature) => checker.getTypePredicateOfSignature(signature)?.type;
    return (
      checker
        .getPropertiesOfType(type)
        .some((property) => next(checker.getTypeOfSymbol(property), positive)) ||
      checker.getIndexInfosOfType(type).some((info) => next(info.type, positive)) ||
      signatures.some(
        (signature) =>
          next(signature.getReturnType(), positive) ||
          (narrowsTo(signature) !== undefined && next(narrowsTo(signature), positive)) ||
          signature.getParameters().some((p) => next(checker.getTypeOfSymbol(p), !positive))
      )
    );
  }

  return {
    checker,
    brandsIn,
    brandOfText,
    handedInBy,
    isLibraryType,
    parameterTypesOf,
    walkContainerPath,
    writableElementsOf,
  };
}

const BRAND_OPTIONS_SCHEMA = {
  type: 'object',
  properties: {
    brands: {
      type: 'array',
      items: {
        type: 'object',
        properties: {
          type: { type: 'string' },
          key: { type: 'string' },
          declaredIn: { type: 'string' },
          maker: { type: 'string' },
          // A brand whose text may hold line breaks; a place that shows one line refuses it.
          multiline: { type: 'boolean' },
        },
        required: ['type', 'key', 'declaredIn', 'maker'],
        additionalProperties: false,
      },
    },
  },
  required: ['brands'],
  additionalProperties: false,
};

function brandNames(brands) {
  return [...brands].map((brand) => brand.type).join(', ');
}

const noBrandForging = {
  meta: {
    type: 'problem',
    docs: {
      description: 'Only the maker module of a brand may claim that a value is of it.',
    },
    schema: [BRAND_OPTIONS_SCHEMA],
    messages: {
      noTypes: 'yagura/no-brand-forging needs type information for this file.',
      assertion:
        'This assertion claims {{brands}} for a value not of it; call the maker ({{makers}}) instead.',
      predicate:
        "A type predicate naming {{brands}} lets unchecked text pass for it; return the maker's value ({{makers}}) instead.",
      declaration:
        'A declaration without a value naming {{brands}} claims a value no maker made ({{makers}}).',
      typeArgument:
        "The type argument {{brands}} names a value the callee makes, not one handed in, so nothing checks it; pass a maker's value ({{makers}}) or take it as a type that holds no brand.",
      writable:
        'This holds {{brands}} in an array, tuple, Set or Map that can be written to, so seen as one of plain text it takes text no maker made ({{makers}}); hold it readonly.',
      writableArgument:
        'This hands {{brands}} in a writable container to a parameter that takes plain text into it, so text no maker made ({{makers}}) can be put in; take the parameter readonly.',
      writableTooDeep:
        'The check cannot see to the end of this type, so it cannot tell whether a brand sits in a writable container; give the inner type a name or hold it readonly.',
    },
  },
  create(context) {
    const [{ brands }] = context.options;
    const services = context.sourceCode.parserServices;
    if (services?.program == null || services.esTreeNodeToTSNodeMap === undefined) {
      return {
        Program(node) {
          context.report({ node, messageId: 'noTypes' });
        },
      };
    }
    const reader = createBrandReader(services.program, brands);
    const { checker } = reader;
    const foreign = brands.filter((brand) => !isFile(context.filename, brand.maker));
    const tsNode = (node) => services.esTreeNodeToTSNodeMap.get(node);

    function foreignBrandsIn(type) {
      return [...reader.brandsIn(type)].filter((brand) => foreign.includes(brand));
    }

    function report(node, messageId, held) {
      context.report({
        node,
        messageId,
        data: {
          brands: brandNames(held),
          makers: [...new Set(held.map((b) => b.maker))].join(', '),
        },
      });
    }

    function checkAssertion(node) {
      const assertion = tsNode(node);
      if (ts.isConstTypeReference(assertion.type)) {
        return;
      }
      const target = checker.getTypeFromTypeNode(assertion.type);
      const source = checker.getTypeAtLocation(assertion.expression);
      const claimed = claimedBrands(checker.getNonNullableType(source), target, 0);
      if (claimed.size > 0) {
        report(node, 'assertion', [...claimed]);
      }
    }

    /** An object type whose members can be set beside another's, one by one. */
    function isPlainObject(type) {
      return (
        (type.flags & ts.TypeFlags.Object) !== 0 &&
        !reader.isLibraryType(type) &&
        !checker.isArrayType(type) &&
        !checker.isTupleType(type)
      );
    }

    /**
     * The foreign brands an assertion of `source` to `target` claims. Where the
     * two are plain objects they are set beside each other member by member and
     * through each signature's result, so a place where the source already holds
     * the brand claims nothing; any other shape is judged as a whole.
     */
    function claimedBrands(source, target, depth) {
      const held = foreignBrandsIn(target);
      const isAny = (source.flags & ts.TypeFlags.Any) !== 0;
      if (held.length === 0 || (!isAny && checker.isTypeAssignableTo(source, target))) {
        return new Set();
      }
      const whole = new Set(held);
      if (isAny || depth > MAX_DEPTH || !isPlainObject(source) || !isPlainObject(target)) {
        return whole;
      }
      if (
        checker.getIndexInfosOfType(target).length > 0 ||
        target.getConstructSignatures().length > 0
      ) {
        return whole;
      }
      const targetCalls = target.getCallSignatures();
      const sourceCalls = source.getCallSignatures();
      if (targetCalls.length > 1 || targetCalls.length !== sourceCalls.length) {
        return whole;
      }
      const claimed = new Set();
      const add = (inner) => inner.forEach((brand) => claimed.add(brand));
      for (const property of checker.getPropertiesOfType(target)) {
        const counterpart = checker.getPropertyOfType(source, property.name);
        if (counterpart === undefined) {
          return whole;
        }
        add(
          claimedBrands(
            checker.getNonNullableType(checker.getTypeOfSymbol(counterpart)),
            checker.getTypeOfSymbol(property),
            depth + 1
          )
        );
      }
      const [targetCall] = targetCalls;
      const [sourceCall] = sourceCalls;
      if (targetCall !== undefined && sourceCall !== undefined) {
        add(claimedBrands(sourceCall.getReturnType(), targetCall.getReturnType(), depth + 1));
      }
      return claimed;
    }

    function checkReturnTypes(node) {
      const declaration = tsNode(node);
      const signature = checker.getSignatureFromDeclaration(declaration);
      if (signature === undefined) {
        return;
      }
      const held = foreignBrandsIn(signature.getReturnType());
      if (held.length > 0) {
        report(node, 'declaration', held);
      }
    }

    // Written or inferred alike: an inferred type argument the arguments do not carry comes
    // from the place the result is put, which no maker checked either.
    function checkTypeArguments(node, resolved) {
      const declaration = resolved.getDeclaration();
      const generic =
        declaration === undefined ? undefined : checker.getSignatureFromDeclaration(declaration);
      const parameters = generic?.getTypeParameters() ?? [];
      if (parameters.length === 0) {
        return;
      }
      const instantiated = checker.getTypeArgumentsForResolvedSignature(resolved) ?? [];
      const explicit = node.typeArguments?.params ?? [];
      parameters.forEach((parameter, index) => {
        const argument = instantiated[index];
        if (argument === undefined || (argument.flags & OPAQUE_FLAGS) !== 0) {
          return;
        }
        const held = foreignBrandsIn(argument);
        if (held.length === 0) {
          return;
        }
        const handedIn = generic
          .getParameters()
          .some((p) =>
            reader.handedInBy(checker.getTypeOfSymbol(p), parameter, true, 0, new Map())
          );
        if (!handedIn) {
          report(explicit[index] ?? node.callee ?? node.tag, 'typeArgument', held);
        }
      });
    }

    function foreignOnly(found) {
      return [...found].filter((brand) => foreign.includes(brand));
    }

    // A type parameter is counted only at a member: a path that stops at a member never
    // reaches the place the generic type is given a brand.
    function checkWritable(node, type, { member }) {
      const { brands: found, tooDeep } = reader.walkContainerPath(type, {
        generics: member,
        never: true,
        inside: false,
      });
      const held = foreignOnly(found);
      if (held.length > 0) {
        report(node, 'writable', held);
      } else if (tooDeep) {
        context.report({ node, messageId: 'writableTooDeep' });
      }
    }

    function writableMembersOf(type) {
      return (type.isUnion() ? type.types : [type]).filter(
        (member) => reader.writableElementsOf(member) !== undefined
      );
    }

    // A writable container of a brand handed to a parameter that holds no such brand in the
    // same place lets the callee fill it with plain text.
    function checkWritableArguments(node, resolved) {
      const parameters = resolved.getParameters();
      node.arguments.forEach((argumentNode, index) => {
        if (argumentNode.type === 'SpreadElement') {
          return;
        }
        const parameter = parameters[Math.min(index, parameters.length - 1)];
        if (parameter === undefined) {
          return;
        }
        // A new `[]` has no other name to be widened through, so `never` is not counted here.
        const held = foreignOnly(
          reader.walkContainerPath(checker.getTypeAtLocation(tsNode(argumentNode)), {
            generics: true,
            never: false,
            inside: false,
          }).brands
        );
        if (held.length === 0) {
          return;
        }
        const takenIn = reader
          .parameterTypesOf(parameter)
          .flatMap(writableMembersOf)
          .flatMap((member) => reader.writableElementsOf(member));
        const takenBrands = takenIn.map(
          (element) =>
            reader.walkContainerPath(element, { generics: true, never: true, inside: true }).brands
        );
        const unbranded = held.filter(
          (brand) => takenIn.length > 0 && !takenBrands.some((found) => found.has(brand))
        );
        if (unbranded.length > 0) {
          report(argumentNode, 'writableArgument', unbranded);
        }
      });
    }

    function checkCall(node) {
      const resolved = checker.getResolvedSignature(tsNode(node));
      if (resolved === undefined) {
        return;
      }
      checkTypeArguments(node, resolved);
      if (node.arguments !== undefined) {
        checkWritableArguments(node, resolved);
      }
    }

    function checkFunction(node, member = false) {
      const signature = checker.getSignatureFromDeclaration(tsNode(node));
      if (signature === undefined) {
        return;
      }
      checkWritable(node.returnType ?? node, signature.getReturnType(), { member });
      const written = node.params.filter(
        (parameter) => !(parameter.type === 'Identifier' && parameter.name === 'this')
      );
      signature.getParameters().forEach((parameter, index) => {
        for (const taken of reader.parameterTypesOf(parameter)) {
          checkWritable(written[index] ?? node, taken, { member });
        }
      });
    }

    // A function written straight into a call or a template is typed by what receives it,
    // and a container it returns ends there or reaches a declaration that is checked.
    function isNamedFunction(node) {
      const parent = node.parent;
      switch (parent?.type) {
        case 'VariableDeclarator':
          return parent.init === node;
        case 'PropertyDefinition':
        case 'MethodDefinition':
        case 'TSAbstractMethodDefinition':
          return parent.value === node;
        case 'Property':
          return parent.value === node && (parent.method || parent.kind !== 'init');
        case 'ExportDefaultDeclaration':
          return true;
        default:
          return false;
      }
    }

    function isFunctionExpression(node) {
      return node?.type === 'FunctionExpression' || node?.type === 'ArrowFunctionExpression';
    }

    // `$: name = …` declares a component's reactive variable, as a top-level `let` does.
    function reactiveVariableOf(node) {
      const { body } = node;
      return body.type === 'ExpressionStatement' &&
        body.expression.type === 'AssignmentExpression' &&
        body.expression.left.type === 'Identifier'
        ? body.expression.left
        : undefined;
    }

    function isAmbient(node) {
      return node.declare === true || (tsNode(node).flags & ts.NodeFlags.Ambient) !== 0;
    }

    // What an ambient member is typed as is claimed with no value behind it.
    function checkAmbientMember(node) {
      if (!isAmbient(node)) {
        return;
      }
      const member = tsNode(node);
      const type =
        ts.isPropertySignature(member) && member.name !== undefined
          ? checker.getTypeAtLocation(member.name)
          : member.type === undefined
            ? undefined
            : checker.getTypeFromTypeNode(member.type);
      const held = type === undefined ? [] : foreignBrandsIn(type);
      if (held.length > 0) {
        report(node, 'declaration', held);
      }
    }

    function isModuleLevel(node) {
      const parent =
        node.parent?.type === 'ExportNamedDeclaration' ? node.parent.parent : node.parent;
      // A component's instance script is a `SvelteScriptElement`, and its top level is the
      // component's own state, as a module's is.
      return parent?.type === 'Program' || parent?.type === 'SvelteScriptElement';
    }

    const visitors = {
      TSAsExpression: checkAssertion,
      TSTypeAssertion: checkAssertion,
      TSTypePredicate(node) {
        if (node.typeAnnotation == null) {
          return;
        }
        const predicated = tsNode(node.typeAnnotation.typeAnnotation);
        const held = foreignBrandsIn(checker.getTypeFromTypeNode(predicated));
        if (held.length > 0) {
          report(node, 'predicate', held);
        }
      },
      VariableDeclaration(node) {
        const ambient = isAmbient(node);
        // A variable inside a function reaches outside only through what the checks of
        // results, members and arguments already see.
        const moduleLevel = isModuleLevel(node);
        for (const declarator of node.declarations) {
          const type = checker.getTypeAtLocation(tsNode(declarator.id));
          const held = ambient ? foreignBrandsIn(type) : [];
          if (held.length > 0) {
            report(declarator, 'declaration', held);
          } else if (moduleLevel && !isFunctionExpression(declarator.init)) {
            checkWritable(declarator.id, type, { member: false });
          }
        }
      },
      SvelteReactiveStatement(node) {
        const variable = reactiveVariableOf(node);
        if (variable !== undefined && tsNode(variable) === undefined) {
          context.report({ node: variable, messageId: 'noTypes' });
        } else if (variable !== undefined) {
          checkWritable(variable, checker.getTypeAtLocation(tsNode(variable)), { member: false });
        }
      },
      PropertyDefinition(node) {
        const type = checker.getTypeAtLocation(tsNode(node));
        const held = isAmbient(node) ? foreignBrandsIn(type) : [];
        if (held.length > 0) {
          report(node, 'declaration', held);
        } else if (!isFunctionExpression(node.value)) {
          checkWritable(node, type, { member: true });
        }
      },
      TSPropertySignature(node) {
        checkAmbientMember(node);
        checkWritable(node, checker.getTypeAtLocation(tsNode(node).name), { member: true });
      },
      TSIndexSignature(node) {
        checkAmbientMember(node);
        const { type } = tsNode(node);
        if (type !== undefined) {
          checkWritable(node, checker.getTypeFromTypeNode(type), { member: true });
        }
      },
      TSMethodSignature(node) {
        checkAmbientMember(node);
        checkFunction(node, true);
      },
      TSCallSignatureDeclaration: checkAmbientMember,
      TSConstructSignatureDeclaration: checkAmbientMember,
      // A function or method without a body: `declare function`, or an overload's signature.
      TSDeclareFunction(node) {
        checkReturnTypes(node);
        checkFunction(node);
      },
      TSEmptyBodyFunctionExpression(node) {
        if (node.parent?.type === 'MethodDefinition') {
          checkReturnTypes(node.parent);
        }
        checkFunction(node);
      },
      FunctionDeclaration(node) {
        checkFunction(node);
      },
      'FunctionExpression, ArrowFunctionExpression'(node) {
        if (isNamedFunction(node)) {
          checkFunction(node);
        }
      },
      CallExpression: checkCall,
      NewExpression: checkCall,
      TaggedTemplateExpression: checkCall,
    };
    // A node the parser made with no TypeScript counterpart has no type to read. A reactive
    // statement is Svelte's own node; its visitor reads the variable it declares.
    return Object.fromEntries(
      Object.entries(visitors).map(([selector, visit]) => [
        selector,
        (node) => {
          if (node.type !== 'SvelteReactiveStatement' && tsNode(node) === undefined) {
            context.report({ node, messageId: 'noTypes' });
          } else {
            visit(node);
          }
        },
      ])
    );
  },
};

// What a template may show besides a brand: values that are not text, and nothing at all.
const NOT_TEXT_FLAGS =
  ts.TypeFlags.NumberLike |
  ts.TypeFlags.BigIntLike |
  ts.TypeFlags.BooleanLike |
  ts.TypeFlags.Null |
  ts.TypeFlags.Undefined;

// Attribute values no one reads or hears, by the value types the HTML attribute index,
// WAI-ARIA 1.2 and SVG 2 give them. Any attribute not named here is checked.
const UNREAD_ON_ANY_ELEMENT = new Set([
  'class',
  'id',
  'style',
  'slot',
  'role',
  'tabindex',
  'dir',
  'lang',
  'translate',
  'hidden',
  'inert',
  'draggable',
  'spellcheck',
  'autocapitalize',
  'autocomplete',
  'inputmode',
  'enterkeyhint',
  'popover',
  // WAI-ARIA states and properties valued by an ID reference, a token, a boolean or a number;
  // the string-valued ones (aria-label, aria-keyshortcuts, ...) are read out.
  'aria-activedescendant',
  'aria-atomic',
  'aria-autocomplete',
  'aria-busy',
  'aria-checked',
  'aria-colcount',
  'aria-colindex',
  'aria-colspan',
  'aria-controls',
  'aria-current',
  'aria-describedby',
  'aria-details',
  'aria-disabled',
  'aria-dropeffect',
  'aria-errormessage',
  'aria-expanded',
  'aria-flowto',
  'aria-grabbed',
  'aria-haspopup',
  'aria-hidden',
  'aria-invalid',
  'aria-labelledby',
  'aria-level',
  'aria-live',
  'aria-modal',
  'aria-multiline',
  'aria-multiselectable',
  'aria-orientation',
  'aria-owns',
  'aria-posinset',
  'aria-pressed',
  'aria-readonly',
  'aria-relevant',
  'aria-required',
  'aria-rowcount',
  'aria-rowindex',
  'aria-rowspan',
  'aria-selected',
  'aria-setsize',
  'aria-sort',
  'aria-valuemax',
  'aria-valuemin',
  'aria-valuenow',
]);
// Custom data and event handlers.
const UNREAD_PREFIXES_ON_ANY_ELEMENT = ['data-', 'on'];

const FORM_CONTROLS = ['input', 'button', 'select', 'textarea', 'fieldset', 'output', 'object'];
const SVG_SHAPE_ELEMENTS = [
  'svg',
  'g',
  'path',
  'circle',
  'ellipse',
  'line',
  'polyline',
  'polygon',
  'rect',
  'use',
  'defs',
  'symbol',
  'clipPath',
  'mask',
  'linearGradient',
  'radialGradient',
  'stop',
];
// SVG shows text as the content of `<title>` and `<text>`, which the text position checks.
const SVG_SHAPE_ATTRIBUTES = [
  'd',
  'cx',
  'cy',
  'r',
  'rx',
  'ry',
  'x',
  'y',
  'x1',
  'y1',
  'x2',
  'y2',
  'width',
  'height',
  'points',
  'pathLength',
  'viewBox',
  'preserveAspectRatio',
  'transform',
  'offset',
  'href',
  'fill',
  'fill-opacity',
  'fill-rule',
  'stroke',
  'stroke-dasharray',
  'stroke-dashoffset',
  'stroke-linecap',
  'stroke-linejoin',
  'stroke-miterlimit',
  'stroke-opacity',
  'stroke-width',
  'opacity',
  'clip-rule',
  'stop-color',
  'stop-opacity',
];
const UNREAD_BY_ELEMENT = new Map();
for (const [elements, names] of [
  [['label', 'output'], ['for']],
  [FORM_CONTROLS, ['name', 'form', 'disabled', 'required', 'readonly', 'multiple', 'autofocus']],
  [
    ['input'],
    [
      'type',
      'list',
      'min',
      'max',
      'step',
      'minlength',
      'maxlength',
      'size',
      'pattern',
      'accept',
      'checked',
    ],
  ],
  // A text area's value is the text being edited, shown as typed.
  [['textarea'], ['value', 'rows', 'cols']],
  // A value sent or measured, not drawn. An option's value is judged by its list.
  [['select', 'button', 'data', 'li', 'meter', 'progress'], ['value']],
  [['option'], ['selected', 'disabled']],
  [['time', 'del', 'ins'], ['datetime']],
  // `download` names the saved file in a dialog, so it is checked.
  [
    ['a', 'area', 'link'],
    ['href', 'target', 'rel', 'hreflang', 'type'],
  ],
  [
    ['img', 'source', 'video', 'audio', 'iframe', 'script'],
    ['src', 'srcset', 'sizes', 'width', 'height', 'type'],
  ],
  [['blockquote', 'q', 'del', 'ins'], ['cite']],
  [['form'], ['action', 'method', 'enctype', 'target']],
  [SVG_SHAPE_ELEMENTS, SVG_SHAPE_ATTRIBUTES],
]) {
  for (const element of elements) {
    const unread = UNREAD_BY_ELEMENT.get(element) ?? new Set();
    names.forEach((name) => unread.add(name));
    UNREAD_BY_ELEMENT.set(element, unread);
  }
}

// An input of these types draws its value as its face.
const BUTTON_INPUT_TYPES = new Set(['button', 'submit', 'reset', 'image']);

// An input's value is the text being edited only when its type is written out and is not a
// button's; a type from an expression or a spread could be either.
function isEditedInputValue(startTag) {
  if (startTag.attributes.some((attribute) => attribute.type === 'SvelteSpreadAttribute')) {
    return false;
  }
  const type = startTag.attributes.find(
    (attribute) =>
      (attribute.type === 'SvelteAttribute' || attribute.type === 'SvelteShorthandAttribute') &&
      attribute.key.name === 'type'
  );
  if (type === undefined) {
    return true;
  }
  if (
    type.type !== 'SvelteAttribute' ||
    !type.value.every((part) => part.type === 'SvelteLiteral')
  ) {
    return false;
  }
  return !BUTTON_INPUT_TYPES.has(
    type.value
      .map((part) => part.value)
      .join('')
      .toLowerCase()
  );
}

// The blocks a template element sits in without them making an element of their own.
const TRANSPARENT_BLOCKS = new Set([
  'SvelteIfBlock',
  'SvelteElseBlock',
  'SvelteEachBlock',
  'SvelteKeyBlock',
  'SvelteAwaitBlock',
  'SvelteAwaitPendingBlock',
  'SvelteAwaitThenBlock',
  'SvelteAwaitCatchBlock',
]);

/** The nearest html element around `element` in the template, or null when none is known. */
function enclosingHtmlElement(element) {
  for (let node = element.parent; node != null; node = node.parent) {
    if (node.type === 'SvelteElement') {
      return node.kind === 'html' ? node : null;
    }
    if (!TRANSPARENT_BLOCKS.has(node.type)) {
      return null;
    }
  }
  return null;
}

/** How attribute `name` of `startTag` is written: one literal, or one expression's source. */
function attributeSpelling(startTag, name, sourceCode) {
  const attribute = startTag.attributes.find(
    (a) =>
      (a.type === 'SvelteAttribute' || a.type === 'SvelteShorthandAttribute') && a.key.name === name
  );
  if (attribute === undefined) {
    return null;
  }
  if (attribute.type === 'SvelteShorthandAttribute') {
    return `expression:${sourceCode.getText(attribute.value)}`;
  }
  if (attribute.value.every((part) => part.type === 'SvelteLiteral')) {
    return `literal:${attribute.value.map((part) => part.value).join('')}`;
  }
  const [only] = attribute.value;
  return attribute.value.length === 1 && only.type === 'SvelteMustacheTag'
    ? `expression:${sourceCode.getText(only.expression)}`
    : null;
}

function htmlElementsNamed(root, name, visitorKeys) {
  const found = [];
  const pending = [root];
  while (pending.length > 0) {
    const node = pending.pop();
    if (node.type === 'SvelteElement' && node.kind === 'html' && node.name.name === name) {
      found.push(node);
    }
    for (const key of visitorKeys[node.type] ?? []) {
      const children = [node[key]].flat();
      children.forEach((child) => typeof child?.type === 'string' && pending.push(child));
    }
  }
  return found;
}

/**
 * True if `datalist` lists the suggestions of an input of this component whose value is the
 * text being edited: picking one makes it that value, so it is shown as the value is (I-1).
 * The two are matched by the spelling of the datalist's `id` and the input's `list`.
 */
function isEditedInputCandidate(datalist, sourceCode) {
  const id = attributeSpelling(datalist.startTag, 'id', sourceCode);
  return (
    id !== null &&
    htmlElementsNamed(sourceCode.ast, 'input', sourceCode.visitorKeys).some(
      (input) =>
        attributeSpelling(input.startTag, 'list', sourceCode) === id &&
        isEditedInputValue(input.startTag)
    )
  );
}

// An option's value is only sent from a select; a datalist shows it as a suggestion.
function isUnreadOptionValue(startTag, sourceCode) {
  const list = enclosingHtmlElement(startTag.parent);
  const owner = list?.name.name;
  return (
    owner === 'select' ||
    owner === 'optgroup' ||
    (owner === 'datalist' && isEditedInputCandidate(list, sourceCode))
  );
}

function isUnreadOn(element, name, startTag, sourceCode) {
  if (
    UNREAD_ON_ANY_ELEMENT.has(name) ||
    UNREAD_PREFIXES_ON_ANY_ELEMENT.some((prefix) => name.startsWith(prefix))
  ) {
    return true;
  }
  if (element === null) {
    return false;
  }
  if (element === 'input' && name === 'value') {
    return isEditedInputValue(startTag);
  }
  if (element === 'option' && name === 'value') {
    return isUnreadOptionValue(startTag, sourceCode);
  }
  return UNREAD_BY_ELEMENT.get(element)?.has(name) ?? false;
}

/** True if no one reads attribute `name` on any of `elements` (`null`: not known which). */
function isUnreadOnAll(elements, name, startTag, sourceCode) {
  return elements === null
    ? isUnreadOn(null, name, startTag, sourceCode)
    : elements.every((element) => isUnreadOn(element, name, startTag, sourceCode));
}

// Markup or text these put into the page is not escaped or checked by the template: an
// iframe's `srcdoc` is read as a document, whatever brand its value has.
const RAW_MARKUP_ATTRIBUTE = 'srcdoc';
const RAW_MARKUP_BINDINGS = new Set(['innerHTML', 'outerHTML', 'textContent', 'innerText']);

const displayText = {
  meta: {
    type: 'problem',
    docs: {
      description:
        'Text a template shows is a brand that says how it was prepared: UiText, DisplayLine or DisplayBlock, and an attribute shows no multiline brand.',
    },
    schema: [BRAND_OPTIONS_SCHEMA],
    messages: {
      noTypes: 'yagura/display-text needs type information for this file.',
      unprepared:
        'This shows {{type}}, which says nothing of how it was prepared; show the UI text from ui-messages or the prepared text from display/untrusted-text.',
      untyped:
        'This shows a value typed {{type}}, so nothing checks how it was prepared; give it the type of the text it holds.',
      multiline:
        'An attribute shows one line, but {{type}} may hold line breaks; prepare it with a one-line maker from display/untrusted-text.',
      rawMarkup:
        '{{name}} puts markup or text into the page past the template, so no brand makes it safe; show the text in the template instead.',
    },
  },
  create(context) {
    const [{ brands }] = context.options;
    const services = context.sourceCode.parserServices;
    if (services?.program == null || services.esTreeNodeToTSNodeMap === undefined) {
      return {
        Program(node) {
          context.report({ node, messageId: 'noTypes' });
        },
      };
    }
    const reader = createBrandReader(services.program, brands);
    const { checker } = reader;

    function membersOf(type) {
      const resolved =
        (type.flags & ts.TypeFlags.Instantiable) !== 0
          ? (checker.getBaseConstraintOfType(type) ?? checker.getUnknownType())
          : type;
      return resolved.isUnion() ? resolved.types : [resolved];
    }

    function typeOf(expression) {
      return checker.getTypeAtLocation(services.esTreeNodeToTSNodeMap.get(expression));
    }

    /** What is wrong with showing a value of `type`, or undefined when nothing is. */
    function problemOf(type, oneLine) {
      const members = membersOf(type);
      if (members.some((member) => (member.flags & OPAQUE_FLAGS) !== 0)) {
        return 'untyped';
      }
      const shown = members
        .filter((member) => (member.flags & NOT_TEXT_FLAGS) === 0)
        .map((member) => reader.brandOfText(member));
      if (shown.some((brand) => brand === undefined)) {
        return 'unprepared';
      }
      return oneLine && shown.some((brand) => brand.multiline === true) ? 'multiline' : undefined;
    }

    function check(expression, oneLine) {
      const type = typeOf(expression);
      const messageId = problemOf(type, oneLine);
      if (messageId !== undefined) {
        context.report({ node: expression, messageId, data: { type: checker.typeToString(type) } });
      }
    }

    /**
     * The elements a start tag may make: its own name, or for `svelte:element` every string
     * literal its `this` can be. `null` when that is not known.
     */
    function elementsOf(startTag) {
      const element = startTag.parent;
      if (element.kind === 'html') {
        return [element.name.name];
      }
      const target = startTag.attributes.find(
        (attribute) => attribute.type === 'SvelteSpecialDirective' && attribute.kind === 'this'
      )?.expression;
      if (target == null) {
        return null;
      }
      if (target.type === 'Literal') {
        return typeof target.value === 'string' ? [target.value] : null;
      }
      // A nullish `this` makes no element.
      const members = membersOf(typeOf(target)).filter(
        (member) => (member.flags & (ts.TypeFlags.Null | ts.TypeFlags.Undefined)) === 0
      );
      return members.length > 0 &&
        members.every((member) => (member.flags & ts.TypeFlags.StringLiteral) !== 0)
        ? members.map((member) => member.value)
        : null;
    }

    function isElementTag(startTag) {
      const element = startTag.parent;
      return (
        element.kind === 'html' ||
        (element.kind === 'special' && element.name.name === 'svelte:element')
      );
    }

    const { sourceCode } = context;

    function isRawMarkupAttribute(startTag, name) {
      return isElementTag(startTag) && name.toLowerCase() === RAW_MARKUP_ATTRIBUTE;
    }

    function reportRawMarkup(node, name) {
      context.report({ node, messageId: 'rawMarkup', data: { name } });
    }

    function checkAttribute(startTag, name, expression) {
      if (
        isElementTag(startTag) &&
        !isRawMarkupAttribute(startTag, name) &&
        !isUnreadOnAll(elementsOf(startTag), name, startTag, sourceCode)
      ) {
        check(expression, true);
      }
    }

    // A spread's names are lowercased as attributes, so an index signature of any string
    // key may write `srcdoc` on an element that may be an iframe.
    function mayWriteRawMarkup(elements, keyType) {
      return (
        (elements === null || elements.includes('iframe')) &&
        checker.isTypeAssignableTo(checker.getStringLiteralType(RAW_MARKUP_ATTRIBUTE), keyType)
      );
    }

    // An index signature whose every key starts with an unread prefix (`data-${string}`).
    function isUnreadIndex(keyType) {
      return (
        (keyType.flags & ts.TypeFlags.TemplateLiteral) !== 0 &&
        UNREAD_PREFIXES_ON_ANY_ELEMENT.some((prefix) => keyType.texts[0].startsWith(prefix))
      );
    }

    // A spread is checked member by member, as the attributes it writes.
    function checkSpread(node) {
      const startTag = node.parent;
      if (!isElementTag(startTag)) {
        return;
      }
      const elements = elementsOf(startTag);
      const type = typeOf(node.argument);
      const report = (messageId, shown) =>
        context.report({ node, messageId, data: { type: shown } });
      for (const member of membersOf(type)) {
        if ((member.flags & OPAQUE_FLAGS) !== 0) {
          report('untyped', checker.typeToString(type));
          return;
        }
        for (const property of checker.getPropertiesOfType(member)) {
          if (isRawMarkupAttribute(startTag, property.name)) {
            reportRawMarkup(node, property.name);
            continue;
          }
          const propertyType = checker.getTypeOfSymbol(property);
          const messageId = isUnreadOnAll(elements, property.name, startTag, sourceCode)
            ? undefined
            : problemOf(propertyType, true);
          if (messageId !== undefined) {
            report(messageId, `${property.name}: ${checker.typeToString(propertyType)}`);
          }
        }
        // An index signature's names are not known, so its values are checked as read.
        for (const info of checker.getIndexInfosOfType(member)) {
          if (mayWriteRawMarkup(elements, info.keyType)) {
            reportRawMarkup(node, `[${checker.typeToString(info.keyType)}]`);
          }
          const messageId = isUnreadIndex(info.keyType) ? undefined : problemOf(info.type, true);
          if (messageId !== undefined) {
            report(
              messageId,
              `[${checker.typeToString(info.keyType)}]: ${checker.typeToString(info.type)}`
            );
          }
        }
      }
    }

    return {
      'SvelteMustacheTag[kind="text"]:not(SvelteStartTag SvelteMustacheTag)'(node) {
        check(node.expression, false);
      },
      'SvelteStartTag > SvelteAttribute > SvelteMustacheTag'(node) {
        checkAttribute(node.parent.parent, node.parent.key.name, node.expression);
      },
      'SvelteStartTag > SvelteShorthandAttribute'(node) {
        if (isRawMarkupAttribute(node.parent, node.key.name)) {
          reportRawMarkup(node, node.key.name);
        } else {
          checkAttribute(node.parent, node.key.name, node.value);
        }
      },
      // Whether written as a literal or an expression.
      'SvelteStartTag > SvelteAttribute'(node) {
        if (isRawMarkupAttribute(node.parent, node.key.name)) {
          reportRawMarkup(node, node.key.name);
        }
      },
      'SvelteStartTag > SvelteSpreadAttribute': checkSpread,
      'SvelteDirective[kind="Binding"]'(node) {
        if (RAW_MARKUP_BINDINGS.has(node.key.name.name)) {
          reportRawMarkup(node, `bind:${node.key.name.name}`);
        }
      },
    };
  },
};

// The parameters of a DOM method that it looks up by as a name, never shown or read out, by
// their names in lib.dom. A name a drawing parameter also has would let text through, so
// adding one takes the user's ruling.
const NAME_PARAMETERS = new Set([
  'selector',
  'selectors',
  'type',
  'qualifiedName',
  'elementId',
  'tagName',
  'contextId',
  'query',
]);

// What the DOM objects are: the document's nodes, the accessibility tree and the window,
// whose alert, confirm and prompt the user agent shows.
const DOM_INTERFACES = ['Node', 'ARIAMixin', 'Window'];
// Input fields are given back the value the user typed, in the form it is sent (I-1).
const EDITED_FIELD_INTERFACES = ['HTMLInputElement', 'HTMLTextAreaElement'];
// What the user agent shows outside the document: a system notification, speech and the
// media session. No door takes a brand for them yet, so a first use is refused until one does.
const UA_SHOWN_INTERFACES = ['Notification', 'SpeechSynthesisUtterance', 'MediaMetadata'];
const UA_SHOWING_METHODS = [
  ['Navigator', 'share'],
  ['ServiceWorkerRegistration', 'showNotification'],
];
// The Tauri modules that put text on the OS: window titles, menus, the tray and dialogs.
const OS_SHOWING_DECLARATIONS =
  /\/node_modules\/@tauri-apps\/(api\/(window|webviewWindow|tray)\.d\.ts$|api\/menu(\.d\.ts$|\/)|plugin-(dialog|notification)\/)/;
// The functions ECMAScript gives for defining or setting the properties of an argument.
const REFLECTIVE_WRITES = [
  ['ObjectConstructor', 'assign'],
  ['ObjectConstructor', 'defineProperty'],
  ['ObjectConstructor', 'defineProperties'],
  ['Reflect', 'set'],
  ['Reflect', 'defineProperty'],
];
// Svelte reads what this renders as HTML and puts it in the DOM.
const SVELTE_RAW_HTML = 'createRawSnippet';
const SVELTE_DECLARATIONS = /\/node_modules\/svelte\//;

/** The name of the interface or namespace `declaration` is a member of. */
function containerName(declaration) {
  const parent = declaration.parent;
  if (parent !== undefined && ts.isInterfaceDeclaration(parent)) {
    return parent.name.text;
  }
  const module = parent !== undefined && ts.isModuleBlock(parent) ? parent.parent : undefined;
  return module !== undefined && ts.isIdentifier(module.name) ? module.name.text : undefined;
}

function isMemberOf(declaration, pairs) {
  const name = declaration.name !== undefined && ts.isIdentifier(declaration.name);
  return pairs.some(
    ([container, member]) =>
      name && declaration.name.text === member && containerName(declaration) === container
  );
}

const domReaders = new WeakMap();

/** What the rule needs of lib.dom in `program`, or null when the program has no DOM. */
function domReaderOf(program) {
  if (!domReaders.has(program)) {
    domReaders.set(program, createDomReader(program));
  }
  return domReaders.get(program);
}

function createDomReader(program) {
  const checker = program.getTypeChecker();
  const interfaceNamed = (file, name) =>
    file.statements.find(
      (statement) => ts.isInterfaceDeclaration(statement) && statement.name.text === name
    );
  // Found by what it declares rather than by its file name.
  const file = program
    .getSourceFiles()
    .find((f) => program.isSourceFileDefaultLibrary(f) && interfaceNamed(f, 'Node') !== undefined);
  if (file === undefined) {
    return null;
  }
  const typesOf = (names) =>
    names
      .map((name) => interfaceNamed(file, name))
      .filter((declaration) => declaration !== undefined)
      .map((declaration) => checker.getTypeAtLocation(declaration.name));
  const domTypes = typesOf(DOM_INTERFACES);
  const editedFieldTypes = typesOf(EDITED_FIELD_INTERFACES);
  const uaShownTypes = typesOf(UA_SHOWN_INTERFACES);
  const [nodeType] = typesOf(['Node']);
  const stringType = checker.getStringType();

  // A type parameter is what its constraint lets in.
  function membersOf(type) {
    const nonNull = checker.getNonNullableType(type);
    return (nonNull.isUnion() ? nonNull.types : [nonNull]).map((member) =>
      (member.flags & ts.TypeFlags.Instantiable) !== 0
        ? (checker.getBaseConstraintOfType(member) ?? checker.getUnknownType())
        : member
    );
  }

  function isOneOf(type, targets) {
    return membersOf(type).some(
      (member) =>
        (member.flags & (OPAQUE_FLAGS | ts.TypeFlags.Never)) === 0 &&
        targets.some((target) => checker.isTypeAssignableTo(member, target))
    );
  }

  /** True if a value of `type` may be text, or nothing says it is not. */
  function holdsText(type, depth = 0) {
    if (depth > MAX_DEPTH || (type.flags & (OPAQUE_FLAGS | ts.TypeFlags.StringLike)) !== 0) {
      return true;
    }
    if (type.isUnionOrIntersection()) {
      return type.types.some((member) => holdsText(member, depth + 1));
    }
    if ((type.flags & ts.TypeFlags.Instantiable) !== 0) {
      const constraint = checker.getBaseConstraintOfType(type);
      return (
        constraint === undefined ||
        checker.isTypeAssignableTo(stringType, constraint) ||
        holdsText(constraint, depth + 1)
      );
    }
    return false;
  }

  /** `holdsText` through the members of objects, as an options bag is read. */
  function holdsTextWithin(type, depth = 0, seen = new Set()) {
    if (holdsText(type) || depth > MAX_DEPTH) {
      return true;
    }
    if (seen.has(type)) {
      return false;
    }
    seen.add(type);
    if (type.isUnionOrIntersection()) {
      return type.types.some((member) => holdsTextWithin(member, depth + 1, seen));
    }
    if (
      (type.flags & ts.TypeFlags.Object) === 0 ||
      type.getCallSignatures().length > 0 ||
      type.getConstructSignatures().length > 0
    ) {
      return false;
    }
    return [
      ...checker.getPropertiesOfType(type).map((property) => checker.getTypeOfSymbol(property)),
      ...checker.getIndexInfosOfType(type).map((info) => info.type),
    ].some((inner) => holdsTextWithin(inner, depth + 1, seen));
  }

  return {
    isDomDeclaration: (declaration) => declaration.getSourceFile() === file,
    isDomObject: (type) => isOneOf(type, domTypes),
    isEditedField: (type) =>
      membersOf(type).every((member) =>
        editedFieldTypes.some((target) => checker.isTypeAssignableTo(member, target))
      ),
    isUaShown: (type) => isOneOf(type, uaShownTypes),
    holdsNode: (type) => nodeType !== undefined && isOneOf(type, [nodeType]),
    isWindowMember: (declaration) => containerName(declaration) === 'Window',
    holdsText,
    holdsTextWithin,
  };
}

const noTextOutsideTemplate = {
  meta: {
    type: 'problem',
    docs: {
      description:
        'Text reaches the DOM only through the template: script writes no text to a DOM object and hands it none.',
    },
    schema: [],
    messages: {
      noTypes: 'yagura/no-text-outside-template needs type information for this file.',
      outsideTemplate:
        'Script must not write text to the DOM or hand it to the DOM; show it in the template, where yagura/display-text checks how it was prepared.',
      outsideDocument:
        'This hands text to the OS or the user agent to show outside the document, and no door checks it yet; design one that takes a prepared brand before showing text here.',
    },
  },
  create(context) {
    const services = context.sourceCode.parserServices;
    const dom =
      services?.program == null || services.esTreeNodeToTSNodeMap === undefined
        ? null
        : domReaderOf(services.program);
    if (dom === null) {
      return {
        Program(node) {
          context.report({ node, messageId: 'noTypes' });
        },
      };
    }
    const checker = services.program.getTypeChecker();
    const tsNode = (node) => services.esTreeNodeToTSNodeMap.get(node);
    const typeOf = (node) => checker.getTypeAtLocation(tsNode(node));

    function memberName(member) {
      if (!member.computed) {
        return member.property.type === 'Identifier' ? member.property.name : undefined;
      }
      return member.property.type === 'Literal' && typeof member.property.value === 'string'
        ? member.property.value
        : undefined;
    }

    function checkWrite(member, written, node, { editedFieldException }) {
      if (member.object.type === 'Super') {
        return;
      }
      if (!dom.holdsText(written)) {
        return;
      }
      const receiver = typeOf(member.object);
      const isEditedValue =
        editedFieldException && memberName(member) === 'value' && dom.isEditedField(receiver);
      if (dom.isDomObject(receiver) && !isEditedValue) {
        context.report({ node, messageId: 'outsideTemplate' });
      } else if (dom.isUaShown(receiver)) {
        context.report({ node, messageId: 'outsideDocument' });
      }
    }

    // A destructuring target is written whatever the source holds, so the target's own type
    // stands for what is written.
    function checkTargets(pattern) {
      switch (pattern?.type) {
        case 'MemberExpression':
          checkWrite(pattern, typeOf(pattern), pattern, { editedFieldException: false });
          break;
        case 'ObjectPattern':
          pattern.properties.forEach((property) =>
            checkTargets(property.type === 'RestElement' ? property.argument : property.value)
          );
          break;
        case 'ArrayPattern':
          pattern.elements.forEach(checkTargets);
          break;
        case 'AssignmentPattern':
          checkTargets(pattern.left);
          break;
        case 'RestElement':
          checkTargets(pattern.argument);
          break;
        default:
          break;
      }
    }

    /** The type a spread argument hands each parameter it fills. */
    function spreadElementType(type) {
      return checker.getIndexInfoOfType(type, ts.IndexKind.Number)?.type ?? checker.getAnyType();
    }

    // A parameter declared `any` takes any value and turns it into text (WebIDL).
    function takesAny(parameter) {
      const type = checker.getTypeOfSymbol(parameter);
      const declared = parameter.valueDeclaration;
      const taken =
        declared !== undefined && ts.isParameter(declared) && declared.dotDotDotToken !== undefined
          ? spreadElementType(type)
          : type;
      return (taken.isUnion() ? taken.types : [taken]).some(
        (member) => (member.flags & ts.TypeFlags.Any) !== 0
      );
    }

    /** Each argument with the parameter it fills; a tagged template's strings are left out. */
    function argumentsOf(node, signature) {
      const parameters = signature?.getParameters() ?? [];
      const [written, offset] =
        node.type === 'TaggedTemplateExpression'
          ? [node.quasi.expressions, 1]
          : [node.arguments ?? [], 0];
      return written.map((argument, index) => ({
        argument,
        parameter: parameters[Math.min(index + offset, parameters.length - 1)],
        type:
          argument.type === 'SpreadElement'
            ? spreadElementType(typeOf(argument.argument))
            : typeOf(argument),
      }));
    }

    function receiverOf(node) {
      const callee = node.type === 'TaggedTemplateExpression' ? node.tag : node.callee;
      return callee.type === 'MemberExpression' && callee.object.type !== 'Super'
        ? callee.object
        : undefined;
    }

    function checkDomCall(node, signature, declaration) {
      const receiver = receiverOf(node);
      const onDomObject = receiver !== undefined && dom.isDomObject(typeOf(receiver));
      const isDom = declaration !== undefined && dom.isDomDeclaration(declaration);
      // A call whose signature cannot be resolved is judged by its receiver alone.
      const counted = isDom
        ? onDomObject ||
          (ts.isFunctionDeclaration(declaration) && ts.isSourceFile(declaration.parent)) ||
          dom.isWindowMember(declaration) ||
          dom.holdsNode(signature.getReturnType())
        : declaration === undefined && onDomObject;
      if (!counted) {
        return;
      }
      const handed = argumentsOf(node, signature).find(
        ({ parameter, type }) =>
          !(isDom && parameter !== undefined && NAME_PARAMETERS.has(parameter.name)) &&
          (dom.holdsText(type) || (parameter !== undefined && takesAny(parameter)))
      );
      if (handed !== undefined) {
        context.report({ node: handed.argument, messageId: 'outsideTemplate' });
      }
    }

    function isOutsideDocumentCall(node, signature, declaration) {
      if (OS_SHOWING_DECLARATIONS.test(slashed(declaration.getSourceFile().fileName))) {
        return true;
      }
      if (!dom.isDomDeclaration(declaration)) {
        return false;
      }
      return (
        isMemberOf(declaration, UA_SHOWING_METHODS) ||
        (node.type === 'NewExpression' && dom.isUaShown(signature.getReturnType()))
      );
    }

    function checkCall(node) {
      const signature = checker.getResolvedSignature(tsNode(node));
      const declaration = signature?.getDeclaration();
      if (declaration !== undefined && isMemberOf(declaration, REFLECTIVE_WRITES)) {
        const [target] = node.arguments;
        if (
          target !== undefined &&
          target.type !== 'SpreadElement' &&
          dom.isDomObject(typeOf(target))
        ) {
          context.report({ node, messageId: 'outsideTemplate' });
        }
        return;
      }
      if (
        declaration !== undefined &&
        ts.isFunctionDeclaration(declaration) &&
        declaration.name?.text === SVELTE_RAW_HTML &&
        SVELTE_DECLARATIONS.test(slashed(declaration.getSourceFile().fileName))
      ) {
        context.report({ node, messageId: 'outsideTemplate' });
        return;
      }
      if (declaration !== undefined && isOutsideDocumentCall(node, signature, declaration)) {
        const handed = argumentsOf(node, signature).find(({ type }) => dom.holdsTextWithin(type));
        if (handed !== undefined) {
          context.report({ node: handed.argument, messageId: 'outsideDocument' });
        }
        return;
      }
      checkDomCall(node, signature, declaration);
    }

    const visitors = {
      AssignmentExpression(node) {
        if (node.left.type === 'MemberExpression') {
          checkWrite(node.left, typeOf(node.operator === '=' ? node.right : node), node, {
            editedFieldException: true,
          });
        } else {
          checkTargets(node.left);
        }
      },
      'ForOfStatement, ForInStatement'(node) {
        if (node.left.type !== 'VariableDeclaration') {
          checkTargets(node.left);
        }
      },
      CallExpression: checkCall,
      NewExpression: checkCall,
      TaggedTemplateExpression: checkCall,
    };
    // A Svelte node with no TypeScript counterpart is not script.
    return Object.fromEntries(
      Object.entries(visitors).map(([selector, visit]) => [
        selector,
        (node) => {
          if (tsNode(node) !== undefined) {
            visit(node);
          }
        },
      ])
    );
  },
};

export default {
  meta: { name: 'eslint-plugin-yagura' },
  rules: {
    'no-brand-forging': noBrandForging,
    'display-text': displayText,
    'no-text-outside-template': noTextOutsideTemplate,
  },
};
